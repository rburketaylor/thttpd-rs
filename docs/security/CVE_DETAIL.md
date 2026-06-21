# Per-CVE Root-Cause Writeups

> One structured writeup per CVE in [`CVE_TABLE.md`](CVE_TABLE.md): what the
> attacker controls, what the C code does with it, why the pattern is unsafe in
> C, and a link to the upstream fix. The companion table
> [`C_PATTERNS.md`](C_PATTERNS.md) gives the `file:line` evidence; this file
> gives the narrative.

---

## CVE-1999-1457 — Buffer overflow in `tdate_parse` via long date string

- **Attacker controls:** an HTTP `Date:`, `If-Modified-Since:`, or similar date
  header value (arbitrary length).
- **What the C code does:** `tdate_parse` in `legacy/src/tdate_parse.c` copies
  the header value into a fixed-size stack buffer before parsing it.
- **Why it is unsafe in C:** no length check before the copy → classic stack
  buffer overflow (CWE-119), remote code execution on 1990s stacks without NX.
- **Class:** CWE-119 (Improper Restriction of Operations within the Bounds of a
  Memory Buffer). CVSSv2 7.5 HIGH.
- **Affected:** thttpd before 2.04-31.
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-1999-1457

## CVE-2001-0892 — `.htpasswd` exposure via chroot + trailing slash

- **Attacker controls:** the trailing `/` of the request URL (one byte).
- **What the C code does:** when chroot is enabled, the auth-check path in
  `auth_check2` (`legacy/src/libhttpd.c` around `:1069`) decides whether to
  enforce `.htpasswd` based on the resolved directory. A trailing slash makes
  the resolver pick a different code path that skips the auth file lookup.
- **Why it is unsafe in C:** the protection is a *runtime convention* layered on
  pointer-based path manipulation, not a type-level guarantee — one boundary
  condition (the slash) collapses it (CWE-668).
- **Class:** CWE-668 (Exposure of Resource to Wrong Sphere). CVSSv2 5.0 MEDIUM.
- **Affected:** Acme thttpd before 2.22 (chroot enabled).
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2001-0892

## CVE-2002-0733 — Reflected XSS in the 404 error page

- **Attacker controls:** the request URL (any string).
- **What the C code does:** the 404 body is formatted with the attacker's URL via
  `err404form = "The requested URL '%.80s' was not found..."` (`libhttpd.c:516`),
  rendered at `:2283,:2290,:2700,:3634` with `hc->encodedurl` — no HTML escaping.
- **Why it is unsafe in C:** string interpolation into an HTML body without
  escaping → reflected XSS (CWE-79). The `%.80s` width cap limits length but
  does not neutralize `<script>`.
- **Class:** CWE-79 (Cross-site Scripting). CVSSv2 7.5 HIGH.
- **Affected:** thttpd 2.20 and earlier.
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2002-0733

## CVE-2004-2628 — Directory traversal on Windows (`%5C..`, drive letters)

- **Attacker controls:** the request URL path (hex-encoded backslashes or
  drive letters).
- **What the C code does:** `de_dotdot` (`legacy/src/libhttpd.c:2395`) collapses
  `..` components, but only recognizes Unix `/` as a separator. On Windows a
  `%5C..` (`\..`) or a `C:` drive prefix bypasses the collapse.
- **Why it is unsafe in C:** the traversal filter is a hand-written string
  rewrite over raw `char*` with no concept of platform separators (CWE-22).
- **Class:** CWE-22 (Path Traversal). CVSSv2 5.0 MEDIUM.
- **Affected:** thttpd 2.07b0.4 on Windows.
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2004-2628

## CVE-2006-1078 — Buffer overflows in the `htpasswd` tool

- **Attacker controls:** a long command-line argument or a long line in a
  password file fed to the separate `htpasswd` helper tool.
- **What the C code does:** `htpasswd` (shipped with acme thttpd) copies the
  argument/line into a fixed-size buffer without a length cap.
- **Why it is unsafe in C:** classic stack/heap buffer overflow (CWE-119).
  Local privilege escalation if `htpasswd` were ever setuid (it normally is not).
- **Class:** CWE-119 (Buffer Overflow). CVSSv3.1 8.4 HIGH. (NVD record:
  NVD-CWE-noinfo; class inferred from advisory text.)
- **Affected:** `htpasswd` in Acme thttpd 2.25b. **Out-of-tree:** `htpasswd.c`
  is not present in this `legacy/` snapshot; cited as a same-family CVE.
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2006-1078

## CVE-2006-1079 — Shell-metacharacter injection in `htpasswd`

- **Attacker controls:** a command-line argument containing shell metacharacters.
- **What the C code does:** `htpasswd` passes the argument to `system()`, which
  invokes a shell.
- **Why it is unsafe in C:** `system()` on attacker-influenced input is
  textbook OS command injection (CWE-78). NVD lists CWE-264; the advisory
  describes the CWE-78 mechanism, so both classes are analyzed in
  `RUST_MITIGATIONS.md`.
- **Class:** CWE-78 (OS Command Injection) / CWE-264. CVSSv2 7.2 HIGH.
- **Affected:** `htpasswd` in Acme thttpd 2.25b. **Out-of-tree** (see above).
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2006-1079

## CVE-2006-4248 — Symlink race in Debian `start_thttpd`

- **Attacker controls:** a predictable temporary filename on a multi-user system.
- **What the C code does:** the Debian `start_thttpd` init wrapper writes to a
  predictable temp path; a local attacker wins the race to symlink that path to
  an arbitrary file, creating or touching it with the daemon's privileges.
- **Why it is unsafe in C/shell:** insecure temporary-file creation (CWE-59 /
  CWE-377). Not a defect in the C daemon itself — `legacy/src/thttpd.c` opens
  its log with a fixed path, not a temp file — but in the packager's wrapper.
- **Class:** CWE-59 (Link Following) / CWE-377 (Insecure Temporary File).
  CVSSv2 7.2 HIGH. (NVD-CWE-noinfo; class inferred.)
- **Affected:** thttpd on Debian GNU/Linux (`start_thttpd` wrapper).
  **Out-of-tree.**
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2006-4248

## CVE-2013-0348 — World-readable `/var/log/thttpd.log`

- **Attacker controls:** a local account on the host.
- **What the C code does:** `re_open_logfile` (`legacy/src/thttpd.c:326`) opens
  the log with `fopen(logfile,"a")` at `:338`, which inherits the process umask
  (typically 022 → 0644, world-readable). The `chmod(logfile, S_IRUSR|S_IWUSR)`
  at `:339` restricts permissions *after* the open, so the file is briefly — and
  on first creation, permanently until rotation — readable by other local users.
- **Why it is unsafe in C:** permission is set as a corrective step rather than
  enforced atomically at open time (CWE-264). The log records request URLs,
  which can carry secrets (query-string tokens, etc.).
- **Class:** CWE-264 (Permissions, Privileges, and Access Controls).
  CVSSv2 2.1 LOW.
- **Affected:** sthttpd before 2.26.4-r2 and thttpd 2.25b.
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2013-0348

## CVE-2017-10671 — Heap overflow in `de_dotdot`

- **Attacker controls:** the request URL path (craftable filename).
- **What the C code does:** `de_dotdot` (`legacy/src/libhttpd.c:2395-2444`)
  rewrites the path in place. The `strcpy( cp + 1, cp2 )` at `:2406` and
  `strcpy( cp2 + 1, cp + 4 )` at `:2425` copy overlapping regions of the
  heap-allocated filename buffer; with a crafted filename the pointer arithmetic
  writes past the allocation.
- **Why it is unsafe in C:** in-place string rewrite over raw `char*` with
  attacker-influenced offsets and no bounds tracking → heap buffer overflow
  (CWE-787). Crash (DoS) or potentially remote code execution.
- **Class:** CWE-787 (Out-of-bounds Write). CVSSv3.1 7.8 HIGH.
- **Affected:** sthttpd before 2.27.1.
- **Fix:** https://github.com/blueness/sthttpd/commit/c0dc63a49d8605649f1d8e4a96c9b468b0bff660
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2017-10671

## CVE-2021-26843 — DoS via overlapping `strcpy` in `de_dotdot`

- **Attacker controls:** the request URL path.
- **What the C code does:** same `de_dotdot` rewrite (`libhttpd.c:2406,:2425`),
  but the crashing path here is the *overlapping* `strcpy` itself: on libcs where
  `strcpy` is implemented over `memcpy`, copying overlapping ranges is undefined
  behavior and crashes the daemon.
- **Why it is unsafe in C:** `strcpy`/`memcpy` semantics forbid overlapping
  operands (CWE-119). The C code has no way to express the no-overlap invariant;
  it relies on the programmer noticing, which is exactly what failed.
- **Class:** CWE-119 (Buffer Overflow / Improper Restriction of Memory
  Operations). CVSSv3.1 7.5 HIGH.
- **Affected:** sthttpd through 2.27.1.
- **Fix:** https://github.com/blueness/sthttpd/issues/14
- **Source:** https://nvd.nist.gov/vuln/detail/CVE-2021-26843
