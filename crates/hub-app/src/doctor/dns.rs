//! System DNS/NSS may block after cancellation; its worker must retain a global slot.

use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
    sync::{Arc, LazyLock},
};
use tokio::sync::Semaphore;

static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(16)));
type Lookup = dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync;

#[derive(Clone)]
pub(super) struct Resolver {
    slots: Arc<Semaphore>,
    lookup: Arc<Lookup>,
}
impl Default for Resolver {
    fn default() -> Self {
        Self {
            slots: Arc::clone(&SLOTS),
            lookup: Arc::new(|host| (host, 0).to_socket_addrs().map(Iterator::collect)),
        }
    }
}
impl Resolver {
    pub async fn addresses(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        if let Ok(ip) = host.parse() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let host = host.to_owned();
        let lookup = self.lookup.clone();
        let (addresses, _permit) = super::local(self.slots.clone(), move || lookup(&host))
            .await
            .map_err(io::Error::other)?;
        Ok(addresses?
            .into_iter()
            .map(|mut address| {
                address.set_port(port);
                address
            })
            .collect())
    }
}
impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let resolver = self.clone();
        Box::pin(async move {
            let addresses = resolver.addresses(name.as_str(), 0).await?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::doctor::CheckDraft;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    #[tokio::test]
    async fn http_and_tcp_share_slots_retained_by_cancelled_dns_workers() {
        let slots = Arc::new(Semaphore::new(1));
        let calls = Arc::new(AtomicUsize::new(0));
        let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
        let (release, wait) = std::sync::mpsc::channel();
        let wait = Mutex::new(wait);
        let count = calls.clone();
        let resolver = Resolver {
            slots: slots.clone(),
            lookup: Arc::new(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                wait.lock().unwrap().recv().unwrap();
                Ok(vec!["127.0.0.1:0".parse().unwrap()])
            }),
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(resolver.clone()))
            .build()
            .unwrap();
        let checks = hub_core::doctor::parse(&[
            CheckDraft::Http {
                name: "HTTP".to_owned(),
                url: "http://dns-fixture.invalid/private".to_owned(),
                expected: 200,
            },
            CheckDraft::Tcp {
                name: "TCP".to_owned(),
                host: "dns-fixture.invalid".to_owned(),
                port: 80,
            },
        ])
        .unwrap();
        let request =
            tokio::spawn(async move { client.get("http://dns-fixture.invalid/").send().await });
        started.recv().await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(resolver.clone()))
            .build()
            .unwrap();
        for check in &checks {
            assert!(
                super::super::network(&client, &resolver, check, Duration::from_secs(1))
                    .await
                    .is_err()
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
