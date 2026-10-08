"""Conforming fixture runtime: forwards every MCP tool result unchanged.

    --reorder               send concurrent results in reverse order (legal; must still PASS)
    --previous-response-id  Responses API only: after the first response, send only new
                            items plus previous_response_id (legal; must still PASS)
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
import bc_agent  # noqa: E402

if __name__ == "__main__":
    sys.exit(bc_agent.run(
        "boundarycheck-fixture-conforming",
        reorder="--reorder" in sys.argv[1:],
        use_previous="--previous-response-id" in sys.argv[1:],
    ))
