"""agent-hub snapshot bridge: present / export / import one Hermes ACP session.

Runs inside Hermes's own Python: through the published launcher
(``hermes --run-module cProfile -o <stats> <this file> ...``) or a console script's interpreter
(``python -I <this file> ...``). The hub reads only the ``--result`` file: cProfile swallows
``SystemExit``, so the exit status means nothing. Nothing is printed: the hub discards both
streams, and a refusal travels as ``{"ok": false, "error": <kind>, "detail": <text>}``.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sqlite3
from typing import Any, Callable

# Hermes refuses more than 25 MiB in one import; a bigger file is not a Hermes export.
PAYLOAD_LIMIT = 64 * 1024 * 1024
DETAIL_LIMIT = 2000


class Refused(Exception):
    """A refusal the hub maps to its own error; ``kind`` is one of the protocol's words."""

    def __init__(self, kind: str, detail: str) -> None:
        super().__init__(detail)
        self.kind = kind
        self.detail = detail


def write_new(path: str, text: str) -> None:
    """A new private file; never follows or replaces an existing one."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as out:
        out.write(text)


def resolve_home(profile: str | None) -> pathlib.Path:
    """The home ``hermes [--profile P] acp`` would use (hermes_cli/main.py _apply_profile_override)."""
    from hermes_constants import get_default_hermes_root

    env = os.environ.get("HERMES_HOME", "").strip()
    if profile is None and env and pathlib.Path(env).parent.name == "profiles":
        return pathlib.Path(env)
    if profile is None:
        active = get_default_hermes_root() / "active_profile"
        try:
            name = active.read_text(encoding="utf-8-sig").strip() if active.exists() else ""
        except (UnicodeDecodeError, OSError):
            name = ""
        if name and name != "default":
            profile = name
    if profile is None:
        return pathlib.Path(env) if env else get_default_hermes_root()
    from hermes_cli.profiles import resolve_profile_env

    try:
        return pathlib.Path(resolve_profile_env(profile))
    except (FileNotFoundError, ValueError) as error:
        raise Refused("profile", str(error)) from error


def check_schema(expected: int, db_path: pathlib.Path) -> None:
    """Both the code's schema and the one recorded in ``state.db`` (if any) must be ``expected``."""
    from hermes_state_common import SCHEMA_VERSION

    if SCHEMA_VERSION != expected:
        raise Refused("schema", f"code schema {SCHEMA_VERSION}")
    if not db_path.exists():
        return
    conn = sqlite3.connect(db_path.resolve().as_uri() + "?mode=ro", uri=True)
    try:
        row = conn.execute("SELECT version FROM schema_version LIMIT 1").fetchone()
    except sqlite3.Error as error:
        raise Refused("schema", "state.db has no readable schema_version") from error
    finally:
        conn.close()
    if row is None or row[0] != expected:
        raise Refused("schema", f"state.db schema {row[0] if row else None}")


def open_db(home: pathlib.Path, *, read_only: bool) -> Any:
    os.environ["HERMES_HOME"] = str(home)
    from hermes_state import SessionDB

    return SessionDB(db_path=home / "state.db", read_only=read_only)


def present(args: argparse.Namespace, home: pathlib.Path) -> dict[str, Any]:
    if not (home / "state.db").exists():
        return {"ok": True, "present": False}
    db = open_db(home, read_only=True)
    try:
        return {"ok": True, "present": db.get_session(args.session) is not None}
    finally:
        db.close()


def export(args: argparse.Namespace, home: pathlib.Path) -> dict[str, Any]:
    if not (home / "state.db").exists():
        return {"ok": True, "found": False}
    db = open_db(home, read_only=True)
    try:
        # Inactive rows too: compaction archives the older segments of the lineage.
        payload = db.export_session_lineage(args.session, include_inactive=True)
    finally:
        db.close()
    if not payload:
        return {"ok": True, "found": False}
    write_new(args.out, json.dumps(payload, ensure_ascii=False, default=str))
    return {"ok": True, "found": True, "segments": len(payload.get("segments") or [payload])}


def with_cwd(segment: dict[str, Any], cwd: str) -> dict[str, Any]:
    """One segment as ``import_sessions`` takes it, resuming in ``cwd`` (``_restore`` reads
    ``model_config.cwd``)."""
    seg = {k: v for k, v in segment.items() if k not in ("segments", "lineage_session_ids", "timings")}
    seg["cwd"] = cwd
    config = seg.get("model_config")
    if isinstance(config, str) and config:
        try:
            config = json.loads(config)
        except ValueError:
            config = None
    if not isinstance(config, dict):
        config = {}
    config["cwd"] = cwd
    seg["model_config"] = config
    return seg


def read_payload(path: str, session: str) -> list[dict[str, Any]]:
    if os.path.getsize(path) > PAYLOAD_LIMIT:
        raise Refused("payload", "payload is too large")
    with open(path, encoding="utf-8") as source:
        try:
            payload = json.load(source)
        except ValueError as error:
            raise Refused("payload", "payload is not JSON") from error
    if not isinstance(payload, dict):
        raise Refused("payload", "payload is not an object")
    segments = payload.get("segments") or [payload]
    if not isinstance(segments, list) or not all(isinstance(s, dict) for s in segments):
        raise Refused("payload", "segments are not objects")
    # The tip must be the last row written: ``present`` looks only for it, so a lineage cut
    # short by a crash can never pass for a finished import.
    if str(payload.get("id") or "") != session or str(segments[-1].get("id") or "") != session:
        raise Refused("payload", "the session is not the tip of the payload")
    return segments


def import_(args: argparse.Namespace, home: pathlib.Path) -> dict[str, Any]:
    segments = read_payload(args.input, args.session)
    db = open_db(home, read_only=False)
    try:
        # One copy only: an existing session is never merged into or shadowed.
        if db.get_session(args.session) is not None:
            raise Refused("exists", "the session is already in state.db")
        # Existing ids (an ancestor kept in this profile) are skipped by Hermes, never rewritten.
        result = db.import_sessions([with_cwd(s, args.cwd) for s in segments])
    finally:
        db.close()
    if not result.get("ok"):
        raise Refused("import", json.dumps(result.get("errors"), ensure_ascii=False))
    if args.session in (result.get("skipped_ids") or []):
        raise Refused("exists", "the session appeared in state.db during the import")
    if args.session not in (result.get("imported_ids") or []):
        raise Refused("import", "the session was not imported")
    return {"ok": True, "imported": result.get("imported", 0), "skipped": result.get("skipped", 0)}


OPERATIONS: dict[str, Callable[[argparse.Namespace, pathlib.Path], dict[str, Any]]] = {
    "present": present,
    "export": export,
    "import": import_,
}


def main() -> None:
    parser = argparse.ArgumentParser(prog="agent-hub-hermes-snapshot")
    parser.add_argument("op", choices=sorted(OPERATIONS))
    parser.add_argument("--result", required=True)
    parser.add_argument("--schema", type=int, required=True)
    parser.add_argument("--session", required=True)
    parser.add_argument("--profile")
    parser.add_argument("--out")
    parser.add_argument("--in", dest="input")
    parser.add_argument("--cwd")
    args = parser.parse_args()
    if args.op == "export" and not args.out:
        parser.error("export needs --out")
    if args.op == "import" and not (args.input and args.cwd):
        parser.error("import needs --in and --cwd")
    try:
        home = resolve_home(args.profile)
        check_schema(args.schema, home / "state.db")
        result = OPERATIONS[args.op](args, home)
    except Refused as refused:
        result = {"ok": False, "error": refused.kind, "detail": refused.detail[:DETAIL_LIMIT]}
    except Exception as error:  # the process boundary: an explicit failed result, nothing printed
        # Only the exception's type: its message may quote session content.
        result = {"ok": False, "error": "internal", "detail": type(error).__name__}
    write_new(args.result, json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
