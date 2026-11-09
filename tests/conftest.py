"""
Pytest configuration for recog tests.

What is left in Python is the shell scripts' tests, the optional ASR server's and
the quality tools'; nothing here may read the developer's installation.
"""
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

# The suite must not depend on the developer's installation config: point both
# the config file and the legacy hosts file at nothing.
os.environ["RECOG_CONFIG_FILE"] = "/nonexistent/recog/config.toml"
os.environ["RECOG_CONFERENCE_HOSTS_FILE"] = "/nonexistent/recog/conference-hosts.json"
