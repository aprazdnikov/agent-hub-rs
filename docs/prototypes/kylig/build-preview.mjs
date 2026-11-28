// Build the same Rust application into an offline, single-file review artifact.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

const directory = fileURLToPath(new URL('.', import.meta.url));
const root = resolve(directory, '../../..');
const run = (tool, args) => execFileSync(tool, args, { cwd: root, stdio: 'inherit' });
const target = 'wasm32-unknown-unknown';
run('cargo', ['build', '--locked', '-p', 'hub-ui-prototype', '--release', '--lib', '--target', target]);
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: root }));
const wasm = join(metadata.target_directory, target, 'release', 'hub_ui_prototype.wasm');
const temp = mkdtempSync(join(tmpdir(), 'hub-ui-preview-'));
try {
  run(process.env.WASM_BINDGEN || 'wasm-bindgen', ['--target', 'web', '--no-typescript', '--out-dir', temp, wasm]);
  const encode = name => gzipSync(readFileSync(join(temp, name)), { level: 9 }).toString('base64');
  const html = readFileSync(join(directory, 'preview.template.html'), 'utf8')
    .replace('__JS_GZIP__', encode('hub_ui_prototype.js'))
    .replace('__WASM_GZIP__', encode('hub_ui_prototype_bg.wasm'));
  writeFileSync(join(directory, 'preview.html'), html);
} finally {
  rmSync(temp, { recursive: true, force: true });
}
