import os
import pathlib


def resolve_profile_env(name):
    path = pathlib.Path(os.environ["STUB_HERMES_ROOT"]) / "profiles" / name
    if not path.is_dir():
        raise FileNotFoundError(f"profile {name!r} does not exist")
    return str(path)
