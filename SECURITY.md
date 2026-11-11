# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub: open the repository's **Security** tab and choose
**Report a vulnerability**. Do not open a public issue, pull request or discussion for them.

Include what you can of:

- the Gono Forum version (`gono-forum-web --version`) and how the site runs (release package or source build,
  MySQL or SQLite, Redis on or off, behind which reverse proxy);
- the steps to reproduce, and what an attacker gains;
- whether the problem is already known to others.

We will acknowledge the report, keep you informed while we work on a fix, and credit you in the release notes
unless you prefer otherwise. Please give us reasonable time to release a fix before disclosing the problem.

## Supported versions

Gono Forum is at 0.1.x. Fixes are made on the latest release; please upgrade before reporting, and check whether the
problem still exists there.

## Deployment notes

Some protections depend on the deployment as described in [docs/install.md](docs/install.md): the application must
listen on loopback behind a reverse proxy on the same machine that sets `X-Real-IP` and `X-Forwarded-Proto`, and the
private configuration with its three keys must stay readable only by the service account. Problems that only appear
when these requirements are not met are still worth reporting if the documentation is unclear, but are not
vulnerabilities in themselves.
