# The Python side of rust/recog-asr-server/tests/gateway_differential.rs:
# `python -m recog.asr_server.gateway` from the given checkout, configured by the
# environment the test sets (ASR_GATEWAY_HOST/PORT, GIGAAM_URL, QWEN_ASR_URL,
# ASR_TIMEOUT_S), as the Rust binary is.
#
#     python -I run_gateway.py <repo>
import runpy
import sys

sys.path.insert(0, sys.argv[1])
runpy.run_module("recog.asr_server.gateway", run_name="__main__")
