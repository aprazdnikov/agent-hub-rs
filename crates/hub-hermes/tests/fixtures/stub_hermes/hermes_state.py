"""Stores sessions in `sessions.json` beside `state.db`; mirrors the contract of
`export_session_lineage` / `import_sessions` (hermes_state_portability.py)."""

import json
import pathlib


class SessionDB:
    def __init__(self, db_path=None, read_only=False):
        self.store = pathlib.Path(db_path).with_name("sessions.json")
        self.read_only = read_only
        self.sessions = json.loads(self.store.read_text()) if self.store.exists() else {}

    def close(self):
        pass

    def get_session(self, session_id):
        return self.sessions.get(session_id)

    def export_session_lineage(self, session_id, include_compacted=False, include_inactive=False):
        if session_id not in self.sessions or not include_inactive:
            return None
        lineage = self.sessions[session_id].get("lineage") or [session_id]
        segments = [self.sessions[i] for i in lineage]
        return {**segments[-1], "segments": segments, "lineage_session_ids": lineage}

    def import_sessions(self, sessions):
        assert not self.read_only
        imported = [s["id"] for s in sessions if s["id"] not in self.sessions]
        skipped = [s["id"] for s in sessions if s["id"] in self.sessions]
        for s in sessions:
            self.sessions.setdefault(s["id"], s)
        self.store.write_text(json.dumps(self.sessions))
        return {"ok": True, "imported": len(imported), "skipped": len(skipped),
                "imported_ids": imported, "skipped_ids": skipped, "errors": []}
