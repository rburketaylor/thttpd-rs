# C-side Vulnerability Patterns

> For each historical CVE class, the specific pattern in `legacy/src/*.c` that
> exhibited the vulnerability. `File:line` references are *structural* — the
> line in our `legacy/` tree that carries the vulnerable pattern class, not the
> exact line the upstream patch touched. The mapping is structural, not temporal.
>
> Every `File:line` below was confirmed with `grep -n` against the current tree
> (Phase 2 automated success criterion).

## CWE → C pattern table

| CWE | Pattern in C | CVE examples | File:line |
|-----|--------------|--------------|-----------|
| **CWE-119 / CWE-787** (Buffer Overflow / OOB Write) | In-place `strcpy` of overlapping ranges in `de_dotdot`; unbounded `strcpy`/`strcat`/`sprintf` into fixed-size struct fields | CVE-2017-10671, CVE-2021-26843, CVE-1999-1457, CVE-2006-1078 | `legacy/src/libhttpd.c:2406` `strcpy( cp + 1, cp2 )` (overlapping); `:2425` `strcpy( cp2 + 1, cp + 4 )` (overlapping); `:1249` `sprintf( to, "%%%02x", ... )`; `:1069,:1121,:1125,:1129,:1131` `strcpy` into `hc->remoteuser` / auth scratch buffers |
| **CWE-125** (Out-of-bounds Read) | `memmove` / `strcpy` with length derived from `strlen` of attacker-influenced filename; reads past allocation when the source and target overlap | CVE-2017-10671, CVE-2021-26843 | `legacy/src/libhttpd.c:2413,:2418,:2422` `memmove( file, file + 2, strlen(file) - 1 )` etc. inside `de_dotdot` |
| **CWE-20** (Improper Input Validation) | `atol`/`atoi` return `0`/`-1` on bad input indistinguishable from valid values; negative `Content-Length` becomes a huge unsigned length | CVE-1999-1457 (tdate), all config-parse CVEs | `legacy/src/libhttpd.c:2191` `hc->contentlength = atol( cp )` (Content-Length — the exact JOURNEY.md negative-Content-Length site); `:3263,:3271` `atoi( cp )` (CGI `Status:` header); `legacy/src/thttpd.c:898,:975,:1047,:1104,:1174` `atoi` for port/max_age/cgi_limit config |
| **CWE-22** (Path Traversal) | URL filename handed to `de_dotdot` which collapses `..` via pointer arithmetic; Windows `%5C..` and drive-letter variants bypass the Unix-only logic | CVE-2004-2628 | `legacy/src/libhttpd.c:2040` `de_dotdot( hc->origfilename )` (the collapse attempt); `:2395` `de_dotdot` definition (the boundary the attacker tries to escape) |
| **CWE-78** (OS Command Injection) | CGI `execve` with a pathname and argv built from the request; `htpasswd`'s `system()` shell call (separate tool) | CVE-2006-1079 | `legacy/src/libhttpd.c:3509` `execve( binary, argp, envp )` (binary derived from `hc->expnfilename`); htpasswd `system()` is in the upstream `htpasswd.c` tool, not in this `legacy/` tree — noted as out-of-tree |
| **CWE-79** (Reflected XSS) | Error-page body formatted with the attacker-controlled URL via `%.80s` | CVE-2002-0733 | `legacy/src/libhttpd.c:516` `err404form = "The requested URL '%.80s' was not found..."`; rendered at `:2283,:2290,:2700,:3634` `httpd_send_err(...,err404form,hc->encodedurl)` — `encodedurl` is attacker-controlled and inserted unescaped |
| **CWE-59 / CWE-377** (Link Following / Insecure Temp File) | `expand_symlinks` trusts symlink resolution; Debian `start_thttpd` init script used a predictable temp path | CVE-2006-4248 | `legacy/src/libhttpd.c:1434` `expand_symlinks(...)` (the C-side symlink walk); CVE-2006-4248 itself is in the Debian `start_thttpd` shell wrapper, not in `legacy/src/*.c` — noted as out-of-tree |
| **CWE-264** (Permissions / Privileges) | Log file opened with default (umask-derived, often 0644) permissions; chroot + trailing-slash exposes dotfiles | CVE-2013-0348, CVE-2001-0892 | `legacy/src/thttpd.c:338` `logfp = fopen( logfile, "a" )` (initial open inherits umask → CVE-2013-0348 world-readable log); `:339` `chmod( logfile, S_IRUSR|S_IWUSR )` (restricts *after* the open, racing the exposure); auth path `legacy/src/libhttpd.c:1069` `strcpy( hc->remoteuser, authinfo )` (the chroot+trailing-slash `.htpasswd` exposure is in the auth-check flow above this) |
| **CWE-668** (Resource Exposure to Wrong Sphere) | Under chroot, a trailing `/` on the request makes the auth check skip the directory's `.htpasswd` | CVE-2001-0892 | `legacy/src/libhttpd.c:1069` and surrounding `auth_check2` flow — the path-rewriting logic that CVE-2001-0892 bypassed with a trailing slash lives in the auth-check region |

## Coverage check

`pipeline/find_unfixed_cves.sh` (Phase 1) scans `legacy/src/*.c` for these
pattern classes and emits every candidate hit. Phase 2's gate is that **every
candidate the scanner reports maps to a row above** — i.e. no uncategorized
risky pattern remains. Run it to regenerate the candidate list:

```bash
bash pipeline/find_unfixed_cves.sh
```

The candidate list is long (thttpd is old C, so `strcpy`/`sprintf`/`strcat`
appear dozens of times). Each appearance is the *same* CWE-119/787 structural
class, covered by the single row above. The per-CVE writeups in
[`CVE_DETAIL.md`](CVE_DETAIL.md) explain the concrete attacker input for each.
