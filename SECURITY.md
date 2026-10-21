# Security policy

## Supported versions

roub has not been released yet. Security fixes land on `main`; once releases start, the latest
minor release will receive fixes as well.

## Reporting a vulnerability

Please do **not** open a public issue, pull request or discussion for a vulnerability.

Report it privately through GitHub:
[Security → Report a vulnerability](https://github.com/janecekvit/roub/security/advisories/new).
Include the affected component and commit, the impact, and steps or a proof of concept to
reproduce it.

You can expect:

- an acknowledgement within 3 working days;
- an assessment and a planned fix date within 10 working days;
- credit in the advisory, unless you prefer to stay anonymous.

Please give us a reasonable time to ship a fix before you disclose the issue publicly. We aim for
90 days at most.

## Scope

roub is a version control system that serves source code across trust boundaries, so these classes
of issues are in scope:

- **Authorization bypass:** reading or writing a repository or directory the caller may not
  access, at any tier, including through `roub-cache` or `roubd` answering by content hash.
- **Integrity:** an object whose content does not match its id being accepted, served or checked
  out; ref updates that skip compare-and-swap.
- **Path handling:** a repository path that escapes the working copy, overwrites files outside it,
  or bypasses the Windows and case-collision rules in `roub-core::path`.
- **Memory safety and denial of service:** panics, unbounded allocation or CPU use triggered by
  untrusted input on the wire, in stored objects or in a git import.
- **Supply chain:** the build, CI workflows, container images and release artifacts.

Bugs in dependencies should be reported upstream as well; tell us too if roub is affected.
