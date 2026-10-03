# Security policy

The Chat with Work Local Agent (`cww`) reads files on people's computers on behalf of a remote server, and changes them in the folders where the user allows changes. We treat any way to read something the user didn't share, to change anything outside a folder that allows changes, to delete anything for good, or to make the daemon do something other than answer its tools, as a serious vulnerability.

## Reporting a vulnerability

Please report vulnerabilities privately:

- Use GitHub's **"Report a vulnerability"** button on this repository (Security ▸ Advisories), or
- Email **hello@chatwithwork.com** (the contact on [chatwithwork.com/security](https://chatwithwork.com/security)) with "Local Agent vulnerability" in the subject.

Please don't open a public issue. Include the cww version (`cww --version`), your OS, and steps to reproduce. A proof of concept against the fake server in `tests/e2e.rs` is ideal.

We will acknowledge your report within 3 working days, keep you updated, and credit you in the advisory unless you prefer otherwise. We ask that you give us a reasonable chance to release a fix before disclosing the issue.

## In scope

- Reading anything outside the shared folders, or anything on the deny list, through the tunnel: path traversal, symlinks, hard links, races, encoding tricks, or parser bugs.
- Getting the daemon to write, move or delete anything outside a folder where the user allowed changes, or on the deny list or the never-changed list there; to delete anything without it going to the system trash; to write anything that runs (a program, a launcher, an execute bit); to execute anything; or to open network connections other than its own tunnel.
- Turning on changes for a folder from the server, or keeping them after the user turned them off.
- Bypassing the daemon's rate and volume limits, pause, or audit log.
- Stealing or replaying device credentials or access tokens, or pairing a device without the user's approval.
- Driving the daemon through its control socket as another local user.
- Memory-safety or denial-of-service bugs triggered by a malicious server or a malicious local file.
- Supply-chain issues in the release process.

## Out of scope

- Reading files the user shared that aren't on the deny list. That is the product working as designed; see the threat model in README.md.
- Attacks that need the user's own account on the machine (for example malware running as the user).
- Vulnerabilities in the Chat with Work web app itself. Report those to the same address, but they are handled separately.

## Supported versions

Only the latest release gets security fixes during the beta.
