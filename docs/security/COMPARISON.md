# Side-by-Side Comparison: C thttpd → thttpd-rs

> For each CWE class, the vulnerable C location and the Rust structure that
> replaces it. This is the worked-evidence companion to
> [`RUST_MITIGATIONS.md`](RUST_MITIGATIONS.md) and [`C_PATTERNS.md`](C_PATTERNS.md).

## Worked example 1 — CWE-787 heap overflow in `de_dotdot` (CVE-2017-10671)

**C** (`legacy/src/libhttpd.c:2406`):

```c
/* Collapse multiple / sequences in place. */
while ( ( cp = strstr( file, "//") ) != (char*) 0 ) {
    for ( cp2 = cp + 2; *cp2 == '/'; ++cp2 )
        continue;
    (void) strcpy( cp + 1, cp2 );     /* overlapping, attacker-influenced */
}
```

The rewrite happens *in place* over a heap `char*` whose length is implicit.
With a crafted filename, `cp + 1` and `cp2` overlap, and on libcs that
implement `strcpy` over `memcpy` the write escapes the allocation. The 2017 fix
patched one shape of overlap; the 2021 CVE-2021-26843 found another shape in
the *same* function.

**Rust** (`rust/crates/thttpd-http/src/url.rs:38`):

```rust
pub fn normalize_path(path: &str) -> Option<String> {
    if path.contains("//") { return None; }            // reject, don't rewrite
    let mut components: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => { components.pop()?; }              // traversal → None
            _ => { components.push(part); }
        }
    }
    // ...
}
```

The function builds a fresh `String` from a typed `Vec<&str>` of components;
there is no destination buffer to overflow and no pointer arithmetic to get
wrong. `..` above root returns `None`, which `?` propagates into a clean
rejection. The historical overlap class is *structurally impossible* — there is
no in-place rewrite.

## Worked example 2 — CWE-20 negative `Content-Length` sentinel

**C** (`legacy/src/libhttpd.c:2191`):

```c
else if ( strncasecmp( buf, "Content-Length:", 15 ) == 0 ) {
    cp = &buf[15];
    hc->contentlength = atol( cp );      /* "-1" → -1, indistinguishable from error */
}
```

`atol` returns `-1` for the string `"-1"`, but the C server treats
`contentlength == -1` as the sentinel meaning "unspecified." A `Content-Length:
-1` header thus silently allocates an effectively unbounded body. (JOURNEY.md
§"three parser edge cases".)

**Rust** (`rust/crates/thttpd-core/src/eventloop.rs:433`):

```rust
slot.http.content_length =
    cl_str.trim().parse::<i64>().ok().filter(|&v| v >= 0);
```

`parse::<i64>()` returns `Result`; `.ok()` discards the parse-error channel
(that error cannot be confused with a value), and `.filter(|&v| v >= 0)` maps
any negative value — including `-1` — to `None`. The sentinel collision cannot
arise because the parse and the range check are distinct, typed operations.

## Worked example 3 — CWE-78 command injection in CGI exec

**C** (`legacy/src/libhttpd.c:3509`):

```c
(void) execve( binary, argp, envp );
```

`execve` itself takes argv as a vector (no shell), but `binary`, `argp`, and
`envp` are all built by `char*` concatenation earlier in the function, and the
sibling `htpasswd` tool (CVE-2006-1079) calls `system()`, which *does* invoke a
shell. The boundary between "value" and "shell token" is a programmer
discipline, not a type distinction.

**Rust** (`rust/crates/thttpd-http/src/cgi.rs:125`):

```rust
let mut cmd = Command::new(script_path);   // no shell
// ...
cmd.env(key, value);                        // env passed as (String, String) values
let mut child = cmd.spawn()?;
```

`std::process::Command` never invokes `/bin/sh -c`; argv and env are separate,
typed values. There is no `system()` anywhere in the workspace (verified:
`grep -rn 'system(' rust/crates/*/src/` returns nothing). Shell-metacharacter
injection is structurally impossible.

## Worked example 4 — CWE-79 reflected XSS in the 404 page

**C** (`legacy/src/libhttpd.c:516` + `:2283`):

```c
static char* err404form =
    "The requested URL '%.80s' was not found on this server.\n";
/* ... */
httpd_send_err( hc, 404, err404title, "", err404form, hc->encodedurl );
```

`hc->encodedurl` (attacker-controlled) is interpolated into the HTML body
unescaped via `%.80s`. The width cap truncates length but does not neutralize
`<script>`.

**Rust** (`rust/crates/thttpd-http/src/response.rs:189` + `:212`):

```rust
fn defang(s: &str) -> String {              // HTML-escape, matching C's defang()
    // '<' -> "&lt;",  '>' -> "&gt;"
}
// ...
let defanged = defang(arg);                  // escaped BEFORE interpolation
let truncated = if defanged.len() > 80 { &defanged[..80] } else { &defanged };
form.replace("%.80s", truncated)
```

The escape is applied to the argument *before* it reaches the format string,
and the helper returns a fresh `String` — the formatter cannot bypass it. The
CVE-2002-0733 vector is closed.

## Summary table

| CWE | C location (vulnerable class) | Rust location (mitigation) | Why the class is closed |
|-----|-------------------------------|----------------------------|-------------------------|
| CWE-119/787 | `libhttpd.c:2406,:2425` `strcpy` overlaps | `url.rs:38` `normalize_path` (fresh `Vec`) | No destination buffer to overflow |
| CWE-125 | `libhttpd.c:2413` `memmove` past allocation | `parse.rs:23` `read_buf[checked_idx]` | Indexing panics on OOB, never reads garbage |
| CWE-20 | `libhttpd.c:2191` `atol(cp)` | `eventloop.rs:433` `parse::<i64>().ok().filter(>=0)` | Parse + range check are distinct typed ops |
| CWE-22 | `libhttpd.c:2395` `de_dotdot` pointer rewrite | `url.rs:38,:50` `normalize_path` → `Option` | Traversal returns `None`, not a corrupted path |
| CWE-78 | `libhttpd.c:3509` `execve` + `system()` | `cgi.rs:125` `Command::new` (no shell) | argv/env are typed values, no `/bin/sh -c` |
| CWE-79 | `libhttpd.c:516,:2283` unescaped `%.80s` | `response.rs:189,:212` `defang()` before replace | Escape is structural, formatter can't bypass |
| CWE-476 | unchecked malloc/`strdup` derefs | `parse_state.rs:6` `enum GotRequest` | `Option<T>` forces null handling at the type level |
| CWE-264 | `thttpd.c:338` umask-derived log perms | `startup.rs:70` audited `initgroups` boundary | Audited FFI; privilege drop ordering enforced |
| CWE-668 | `libhttpd.c:1069` `char*` auth path | `thttpd-auth/lib.rs:40` typed `Path` walk | No `char*` boundary for trailing-`/` to collapse |
