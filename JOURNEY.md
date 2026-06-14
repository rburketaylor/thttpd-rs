# Case Study: Porting thttpd from C to Rust

This is a record of what was learned porting `sthttpd 2.27.0` from C to Rust and proving the port against the original binary. It is not a progress log. The phases, dates, and checkmarks that an implementation diary would carry have been left out on purpose; what remains is the set of methodological lessons the project actually turned on, each grounded in something concrete that happened during the migration.

The short version: a port that its own plan called "fully implemented" did not respond to requests, and the path from there to a server characterized by 105 side-by-side differential scenarios is the part worth reading. The README covers what the project is and how to run it; this document covers why the method matters.

## Context

`thttpd` is a small, single-threaded, event-driven HTTP server written in C roughly three decades ago. The goal was a Rust port with provable parity, not a redesign. The approach was fixed before any port code was written:
keep the C binary in the tree as an executable specification, capture representative behavior before changing anything, port behind clear module boundaries, run old and new implementations side by side, and track normalization and known deviations explicitly. Everything that follows is a consequence of that approach, or a correction forced by violating it.

## "Implemented" is not a behavior

The first attempt at the port ran a multi-phase pipeline (discover, research, design, plan, implement, validate) and reported all 22 implementation phases as complete. The gates were structural: a file existed, it compiled, a test was collected. The plan's exit criteria had been written to be mechanically checkable, and the implementation met the letter of every one.

Running the result told a different story. Rust unit tests passed, but the server did not answer requests. The event loop dispatched nothing. The harness errored at fixture setup because the C reference binary had never been compiled. The golden-capture scripts printed "placeholder." The implementation had satisfied the gates and built nothing that worked.

The fix was not more implementation; it was better gates. Every subsequent gate was restated as an observable behavior: `curl localhost:19997/index.html` returns the file body, the C binary serves files on port 19998, the harness sends a real HTTP request and asserts on a real response, the golden capture writes `baseline.json`. Structural completion is cheap to verify and meaningless; behavioral completion is the only thing that matters.

This is the central lesson of the project, and it shaped every section that follows.

## The legacy binary is the specification

Behavior was captured from the C binary before any Rust dispatch code was written. Forty-five representative requests went in; forty-five response records came out, each with status line, header block, and body. Those captured records became the contract. There was never a meeting about what "correct" meant, because correctness had already been defined as "whatever the C binary did."

This is the only reason the later repair work was tractable. Every disputed behavior, from the byte order of a directory listing to whether a malformed request returns 400 or 501, was settled by replaying the request against the reference and reading the response. Without the golden master, each of those questions would have become a design discussion.

## Differential testing at scale

Once the harness existed, adding a test cost one request. A single raw-socket HTTP request was replayed against both servers, and the comparison engine produced an eight-field verdict automatically. The investment was front-loaded into the harness; after that, coverage grew cheaply.

The first full differential run passed 2 of 45 scenarios. The 43 failures were not random; they fell into categories, and each category closed systematically:

- Missing response headers. The C server returned seven headers; the Rust server returned four. `Last-Modified`, `Accept-Ranges`, `Connection: close`, and the `charset=iso-8859-1` parameter on `Content-Type` were all absent.
- Missing features. `If-Modified-Since` did not return 304. `Range` did not return 206. `HEAD` included a body. HTTP/0.9 requests got a full header block instead of a raw response. Unknown methods did not return 501.
- Security gaps. Symbolic links were not checked against the web root, so a symlink could escape. Directory traversal was not detected. A permission-denied file was reported as not found rather than forbidden.
- CGI output. Status-header extraction, NPH-script handling, and the header/body split in CGI output were all wrong. This category also produced the `dual_server_process` session fixture that runs both servers side by side for the rest of the suite.
- Nondeterminism. `Date` headers, minor `Server` differences, and dynamically allocated ports varied between processes. Rather than mark these as matches, the comparison engine grew explicit, documented normalizers, so that only behavioral differences can fail a scenario.

After this loop, 71 of 71 fast differential scenarios passed. The remaining 9 were not failures of comparison; they were failures of the Rust server itself, and they had been deferred because two of them crashed the process.

## Where the bugs hid: three parser edge cases

The last nine scenarios were closed in a single focused session. Three bugs were found and fixed, and each is worth reading closely because it is the kind of bug that unit tests do not catch and differential tests do.

**The CGI stdin deadlock.** When a request carried no `Content-Length` header (notably `Transfer-Encoding: chunked`), the server never closed the stdin pipe to the CGI child. The child's `cat` blocked reading stdin; the server blocked reading the child's stdout. This is a textbook pipe deadlock. The fix was to always take and drop the stdin pipe, writing body bytes into it only when a body was actually present. (`cgi.rs`)

**Negative `Content-Length`.** The header value `"-1"` parsed as `Some(-1)`, which was then cast to `usize` and wrapped to `MAX_USIZE`. The C server's `atol()` also returns `-1` for the string `"-1"`, but C treats `contentlength == -1` as a sentinel meaning "unspecified," whereas the Rust port had silently allocated an effectively unbounded body. The fix was to filter negative values to `None`. (`eventloop.rs`)

**Incremental FSM state reset.** The request parser reset to its initial `FirstWord` state on every call instead of resuming from the stored `parse_state`. For a normal request, where the full request arrives in one read, this is invisible. For a slow-loris request, where bytes trickle in one at a time, the parser could never accumulate enough state to recognize a complete request line. The C server handled this naturally because `hc->checked_state` lives on the connection struct and persists across reads. The Rust port had to be taught the same discipline: the stored `parse_state` is now passed in as the initial state on every call. (`parse.rs`)

The shared shape of these three bugs is the lesson. They are all edge cases in how input is delivered or sized: no Content-Length, negative Content-Length, byte-by-byte delivery. None of them surfaces in a unit test that constructs a well-formed request and hands it over in one piece. All of them surface immediately under a differential harness that replays the same bytes against an implementation that got them right thirty years ago.

## The oracle needs its own tests

The differential engine became part of the trusted computing base, and that created a second-order risk: if the comparator was wrong, every passing scenario was suspect. It was wrong. The normalized comparison profile used to mark `body_sha256` as matched unconditionally, meaning two responses with completely different bodies could pass a scenario as long as their headers lined up.

That bug is fixed. Normalized mode now hashes the normalized body and fails on any remaining body mutation. Exact and normalized profiles are explicit and documented field by field, and 63 comparator unit tests run in CI alongside the differential scenarios. The lesson is direct: a tool whose only job is to detect drift has to be tested for drift in itself.

## Operational details are not a polish phase

A port that gets every request right but reorders privileged operations is not actually a port. Several behaviors that are easy to dismiss as operational housekeeping are observable, and someone depends on each of them:

- Listener sockets bind after `chroot` and before `setuid`, matching the legacy ordering. Binding after dropping privileges fails; binding before chroot exposes the wrong filesystem.
- The `-C` flag parses supported legacy config directives and rejects unknown or unsupported ones with an actionable error rather than silently ignoring them.
- The configured pidfile is written on successful startup.
- An unreadable `.htpasswd` returns the legacy 403, not a 401 that the legacy server never produced.

These shipped inside the migration, not in a deferred polish phase, because they are part of the contract the reference implementation established.

## Where it stands

One hundred and five differential scenarios now characterize externally observable request behavior against the C reference, with normalization documented field by field. The full test inventory, build prerequisites, and the one-command gate (`make verify`) are in the README.

The server is not described as a full operational drop-in replacement. Throttle enforcement, daemonization, request logging, CGI resource controls, IPv6 listeners, and several CLI and config surfaces are not yet at parity, and each is recorded with its legacy behavior, current Rust behavior, impact, and disposition in `docs/KNOWN_DEVIATIONS.md`. An explicit list of known gaps is more useful than an unqualified compatibility claim; it is what allows the 105 passing scenarios to mean what they appear to mean.
