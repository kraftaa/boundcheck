# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately, not in a public issue:
use **GitHub private vulnerability reporting** on this repository
(*Security* tab → *Report a vulnerability*).

Please include the boundarycheck version (`boundarycheck --version`), the
platform, the command line, and a minimal reproduction. Do not include real
API keys or customer data in reports or artifacts.

## Supported versions

Only the latest release receives security fixes.

## Threat model

boundarycheck is a test harness for **controlled test agents**. It starts the
command you give it, on your machine, as your user.

In scope (please report):

- a way for input on the fake provider's socket, the MCP server's stdin, an
  adapter manifest, or a report/artifact path to crash boundarycheck, make it
  write outside its work or artifacts directory, or follow a planted symlink;
- secrets reaching reports or artifacts without an explicit unsafe opt-in
  (headers, argv, inherited environment, or runtime logs);
- a false PASS: boundarycheck reporting PASS although the provider-visible
  content differs from what the MCP server emitted;
- child processes surviving a run on Linux, or on macOS while their parent
  process is alive.

Out of scope (documented limitations):

- a runtime that is deliberately written to deceive the harness: it runs as
  the same OS user and launches the MCP server, so it can reach every file and
  socket the harness uses (`--isolation docker` protects the host, not the
  integrity of the test);
- denial of service by the runtime under test against its own run.
