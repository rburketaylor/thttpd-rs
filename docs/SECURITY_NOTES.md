# Security Notes

thttpd-rs is a parity-focused port, not a claim that a 1990s CGI execution model
is a modern sandbox.

## Audited Unsafe Boundaries

The workspace avoids unsafe code in request parsing and protocol handling. The
remaining unsafe operations are narrow operating-system or C-library boundaries:

- `crypt(3)` for legacy `.htpasswd` hash compatibility
- signal registration
- memory mapping and Unix account initialization

Each boundary must document pointer, lifetime, serialization, and return-value
assumptions next to the unsafe block. `cargo clippy -D warnings`, `cargo audit`,
and `cargo deny` run in the verification workflow.

## CGI Risk

The current CGI path starts a child with cleared environment variables, pipes
stdin/stdout/stderr, writes the request body, and closes stdin to avoid the
legacy deadlock discovered during differential testing.

It does not yet provide:

- execution timeouts and forced termination
- bounded stdout/stderr capture
- operating-system resource limits
- filesystem or syscall sandboxing
- runtime concurrency enforcement for parsed `cgilimit`

These are recorded as operational deviations rather than hidden behind the
request-parity result. A hardened deployment should use a supervisor or sandbox
until native controls are implemented and tested.

## Privilege Ordering

Startup performs chroot, binds listeners, then drops group and user privileges.
This preserves the legacy requirement that privileged ports be opened before
setuid while keeping filesystem confinement in place first.
