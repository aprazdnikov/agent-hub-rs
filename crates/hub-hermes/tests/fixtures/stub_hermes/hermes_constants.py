import os
import pathlib


def get_default_hermes_root():
    return pathlib.Path(os.environ["STUB_HERMES_ROOT"])
