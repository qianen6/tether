"""Check every recorded frame against its specific local stable ACP v1 type.

Uses the already-installed jsonschema package; all references stay in $defs.
The permissive ProtocolLevel notification branch of the root schema is not
used, so a malformed session/update cannot pass as an extension notification.
"""

import json
from pathlib import Path

from jsonschema import Draft202012Validator


ROOT = Path(__file__).resolve().parents[2]
SCHEMA = json.loads(
    (ROOT / ".ref/agent-client-protocol/schema/v1/schema.json").read_text(
        encoding="utf-8"
    )
)
METHODS = {
    "initialize": "InitializeRequest",
    "session/update": "SessionNotification",
    "session/request_permission": "RequestPermissionRequest",
    "session/cancel": "CancelNotification",
}


def response_type(name, message):
    if "error" in message:
        return "Error", message["error"]
    if name == "decisions.client.jsonl":
        return "RequestPermissionResponse", message["result"]
    if name == "initialize.agent.jsonl" or (
        name == "resume-unsupported.jsonl" and message["id"] == 1
    ):
        return "InitializeResponse", message["result"]
    if name == "new-session.agent.jsonl" or name == "resume-unsupported.jsonl":
        return "NewSessionResponse", message["result"]
    if name == "load-session.jsonl":
        return "LoadSessionResponse", message["result"]
    return "PromptResponse", message["result"]


def validate():
    validators = {}
    count = 0
    files = sorted((ROOT / "fixtures/acp").glob("*.jsonl"))
    assert files, "no ACP fixtures"
    for path in files:
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            message = json.loads(line)
            try:
                assert isinstance(message, dict)
                assert message["jsonrpc"] == "2.0"
                if "id" in message:
                    assert type(message["id"]) in (int, str)
                if "method" in message:
                    assert "result" not in message and "error" not in message
                    kind, body = METHODS[message["method"]], message["params"]
                else:
                    assert "id" in message
                    assert ("result" in message) != ("error" in message)
                    kind, body = response_type(path.name, message)
                if kind not in validators:
                    validators[kind] = Draft202012Validator(
                        {
                            "$schema": SCHEMA["$schema"],
                            "$defs": SCHEMA["$defs"],
                            "$ref": f"#/$defs/{kind}",
                        }
                    )
                validators[kind].validate(body)
            except Exception as error:
                raise AssertionError(f"{path.name}:{line_number}: {error}") from error
            count += 1
    print(f"validated {count} ACP v1 frames in {len(files)} JSONL fixtures")


if __name__ == "__main__":
    validate()
