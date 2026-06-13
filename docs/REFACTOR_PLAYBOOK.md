# Refactor Playbook Demonstrated by thttpd-rs

This project treats legacy modernization as a risk-management exercise rather
than a translation exercise.

## 1. Inventory the Existing System

Map executable entry points, configuration, external protocols, security
boundaries, operational behavior, and dependencies. The `knowledge/` records
connect each C module to its Rust replacement.

## 2. Define Observable Behavior

Identify what callers and operators can observe: response bytes, status codes,
header order, connection lifecycle, CLI/config behavior, privilege transitions,
logging, signals, and resource limits. Internal similarity is not the contract.

## 3. Build Characterization Tests

Run real requests against the legacy binary before trusting the replacement.
Fixtures must be validated against the old system so a broken fixture cannot
produce false confidence.

## 4. Make the Oracle Trustworthy

Test the comparison engine itself. Exact and normalized fields must be explicit;
normalization must be narrow, documented, and covered by mutation tests.

## 5. Create Stable Boundaries

Split the replacement along existing responsibilities. Port leaf modules first
and preserve the event model until parity is established.

## 6. Compare Old and New Side by Side

Send identical inputs to both implementations and compare observable outcomes.
Every discovered mismatch becomes either a regression test and fix or a recorded
deviation with an owner and disposition.

## 7. Preserve Operational Semantics

Compatibility includes configuration files, startup ordering, privilege drop,
signals, logging, pidfiles, timeouts, and rollback behavior. Request parity alone
does not justify a drop-in replacement claim.

## 8. Gate the Cutover

Use one reproducible command in development and CI. Keep the old implementation
available until parity gates, operational checks, and rollback procedures pass.

## Applying This to an Internal Business System

- Capture current workflows, reports, permissions, exports, and edge cases.
- Build fixtures from representative historical cases with sensitive data removed.
- Compare old and new results in shadow mode before moving write traffic.
- Record intentional changes separately from accidental deviations.
- Migrate in small slices behind stable interfaces or routing controls.
- Keep rollback simple until production evidence supports full cutover.
- Modernize data models and workflows only after compatibility is measurable.
