# Apache License 2.0

## Context

Kapto is an open-source tool that teams install on their own machines and
build on. The licence should let anyone use, modify and redistribute it,
including inside closed products, while protecting contributors and users
from patent claims.

## Approach

All of Kapto is licensed under the **Apache License 2.0**: code, build files,
sample robot packages and documentation. Contributions are accepted under the
same licence. Dependencies must have permissive licences, which the
dependency check enforces.

## Consequences

- Anyone may use, modify and redistribute Kapto, including in closed products.
- Every contributor grants a patent licence for their contributions, and
  anyone who sues over patents in Kapto loses that licence.
- Redistributions keep the `NOTICE` file, and modified files carry a notice
  that they were changed.
- The licence grants no rights to the Kapto name.
- Modified versions don't have to be published as open source.
- Dependencies under copyleft licences can't be used.

## Alternatives considered

- **MIT, or MIT and Apache-2.0 together:** common for Rust, but MIT has no
  patent grant.
- **MPL-2.0:** requires changes to Kapto's own files to be published, at the
  cost of being less permissive.
- **GPL:** requires whole derived works to be published, which would keep
  many companies from using Kapto.
