use super::*;
#[cfg(unix)]
use crate::PathDisplayExt;

/// Hard cap on bytes the IPC client will read from a single daemon response.
///
/// Prevents a malicious or hijacked socket from streaming gigabytes of payload
/// and OOMing the CLI. `/status` and `/drift` responses in normal operation
/// are O(100s) of bytes for a healthy daemon and a few KiB for a heavily-drift
/// daemon — 256 KiB leaves three orders of magnitude of headroom.
pub(crate) const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

/// Judge the path that reaches `dir`, then create `dir` (and any missing
/// parents) with mode 0700 and verify the result is owner-private AND owned by
/// the running euid.
///
/// Used by `run_health_server` to guarantee the IPC socket cannot be dropped
/// into a location another account can reach. Refuses to proceed if the final
/// mode has any group/other bits set: an attacker with `+w` on the parent could
/// rename the socket and substitute theirs, defeating the 0600 set on the
/// socket itself.
///
/// The ORDER is load-bearing. [`refuse_swappable_path`] runs against the
/// deepest component that already exists, before anything is created or
/// chmodded, because both of those mutations work by PATH and a path-based
/// mutation resolves every ancestor through whatever stands there: a create
/// under an ancestor another account re-pointed makes a root-owned directory
/// wherever they aimed it, and a path-based chmod under one lands 0700 on a
/// directory that already existed, locking its own owner out.
/// `O_NOFOLLOW` answers for the FINAL component alone and cannot answer for
/// either. The missing tail is then made one component at a time with
/// `mkdir(2)`, which never follows a final symlink and answers `EEXIST` for a
/// component that appeared since the walk, so every component this call adds is
/// one it made itself and the walk's verdict covers the whole path.
///
/// The mode half covers the umask-leak case (mkdir under default 0o022 leaving
/// 0755) as well as operator-pre-created directories with the wrong perms. The
/// OWNER half covers what the mode alone admits: a directory at 0700 owned by an
/// unprivileged user is fully writable BY that user, and this daemon's runtime
/// directory resolves under `$XDG_RUNTIME_DIR` or `$HOME`, so a root daemon
/// started in that user's session would let its owner unlink the bound socket
/// and plant a symlink there for the chmod in `run_health_server` to follow. An
/// unprivileged user pretending to be root is a different question and stays out
/// of the local-daemon threat model, root being trusted on the host already.
#[cfg(unix)]
pub(crate) fn ensure_owner_private_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let refuse = |message: String| DaemonError::HealthSocketError { message };
    let dir = crate::absolutize_path(dir);
    let euid = nix::unistd::geteuid().as_raw();

    // The deepest component that already exists is found with
    // `symlink_metadata` rather than an `exists` probe, which follows: a
    // dangling link is exactly the component an attacker plants, and a
    // following probe walks straight past it and hands the tail's `mkdir` the
    // target they chose.
    let (existing, existing_meta) = dir
        .ancestors()
        .find_map(|p| p.symlink_metadata().ok().map(|meta| (p, meta)))
        .ok_or_else(|| refuse(format!("stat parent {}: no component exists", dir.posix())))?;

    refuse_swappable_path(existing, euid)?;

    // Nothing left to create means the leaf already stood there, so its KIND is
    // settled here, by a decision this function words, rather than by whatever
    // errno the chmod below happens to return for it.
    if existing == dir.as_path() {
        // The leaf is the one component this function MUTATES, and that is where
        // the walk's admission of a link component stops: a path-based chmod of
        // a link lands on the link's TARGET, so admitting a leaf link would aim
        // the 0o700 at a directory the operator never named. The walk has
        // already followed this link and approved where it leads, and approving
        // a directory to bind under is not agreeing to rewrite its mode.
        if existing_meta.file_type().is_symlink() {
            return Err(refuse(format!(
                "refusing to bind: parent directory {} is a symlink, and the 0700 this \
                 needs would land on whatever it points at rather than on a directory \
                 cfgd made",
                dir.posix()
            ))
            .into());
        }
        // The link arm above has returned, so this asks only about a real file:
        // the no-follow open refuses a symlink but opens a regular file happily,
        // and a file standing where the directory belongs would have its own
        // mode rewritten instead of being refused.
        if !existing_meta.is_dir() {
            return Err(refuse(format!(
                "refusing to bind: parent directory {} is not a directory",
                dir.posix()
            ))
            .into());
        }
    }

    let tail = dir
        .strip_prefix(existing)
        .unwrap_or(std::path::Path::new(""));
    let mut cursor = existing.to_path_buf();
    for part in tail.components() {
        cursor.push(part);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&cursor)
            .map_err(|e| {
                // This loop only creates components the walk just found ABSENT,
                // so an `EEXIST` is never an operator's pre-created directory:
                // it is a component that appeared between the judgment and this
                // `mkdir(2)`, which no verdict covers. Distinguishing the two
                // needs no second syscall, only this errno.
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    return refuse(format!(
                        "create parent {}: a component that did not exist when this path \
                         was judged appeared before it could be created; refusing rather \
                         than using it",
                        cursor.posix()
                    ));
                }
                refuse(format!("create parent {}: {}", cursor.posix(), e))
            })?;
    }

    // The mode is still set explicitly: `mkdir(2)` masks its argument with the
    // process umask, so a host umask withholding the owner bits would leave a
    // directory this daemon cannot traverse.
    crate::set_file_permissions_nofollow(&dir, 0o700)
        .map_err(|e| refuse(format!("chmod parent {}: {}", dir.posix(), e)))?;
    // This read FOLLOWS, which two guards established above make safe. No
    // untrusted account can swap a component of this path, every shape that
    // would let one having been refused by the walk; and a symlink AT the leaf
    // was refused either by the kind check near the top of this function (a leaf
    // that already stood there) or by the no-follow chmod immediately above (one
    // swapped in after this call created it), so the inode this stats is the
    // inode that was chmodded.
    let meta = std::fs::metadata(&dir)
        .map_err(|e| refuse(format!("stat parent {}: {}", dir.posix(), e)))?;
    let mode = meta.permissions().mode() & 0o777;
    if !is_owner_private_mode(mode) {
        return Err(refuse(format!(
            "refusing to bind: parent directory {} is not owner-private (mode {:o})",
            dir.posix(),
            mode
        ))
        .into());
    }
    if meta.uid() != euid {
        return Err(refuse(format!(
            "refusing to bind: parent directory {} is owned by uid {} rather than uid {}",
            dir.posix(),
            meta.uid(),
            euid
        ))
        .into());
    }
    Ok(())
}

/// Whether `mode`'s permission bits are owner-private: no group bit and no
/// other bit set.
///
/// The one mask [`ensure_owner_private_dir`] refuses a socket directory with,
/// named so a pin can ask it. The arm it guards cannot be entered end to end
/// under uid 0, where the chmod always lowers the mode before the re-stat reads
/// it, so this predicate is the only surface the mask is observable on.
#[cfg(unix)]
pub(crate) fn is_owner_private_mode(mode: u32) -> bool {
    mode & 0o077 == 0
}

/// Refuse a socket directory whose path some other account could re-point
/// between this check and the path-based chmod in `run_health_server`.
///
/// `ensure_owner_private_dir`'s two refusals answer for the directory's own
/// inode, and everything around them works by PATH: the create, the chmod, then
/// `exists`, `remove_file`, `bind` and `set_permissions`. A final-component
/// check therefore leaves the ancestors open, which is the whole capability in
/// this module's own threat model: the unprivileged owner of `$HOME` and
/// `$HOME/.cache` lets the root daemon create `…/cfgd/runtime` (root-owned,
/// 0700, both refusals satisfied), then renames that component and drops a link
/// of their own in its place, and every later path-based operation resolves
/// through the link. Closing the capability is what this walk does; narrowing
/// the race would only shorten it. Its caller runs it BEFORE any mutation, for
/// the reason stated there.
///
/// The walk runs from the ROOT down rather than in `Path::ancestors`' own order,
/// because a component's verdict means nothing until every component above it is
/// known unswappable, and each one is read with `symlink_metadata` so a link is
/// seen as a link rather than followed. Three facts per component, asked in this
/// order:
///
/// 1. OWNERSHIP: the component is owned by this euid, or by uid 0. Demanding
///    this euid alone would refuse `/` and `/home`, root-owned on every host, so
///    no user-scope daemon could ever start; and root is trusted here already:
///    it can chown, chmod and unlink anything this socket protects. The socket's
///    OWN directory is held to the stricter `uid == euid` by the caller above:
///    cfgd binds in a directory of its own, never in one root merely lent it.
/// 2. LINK COMPONENTS: a link is admitted and the walk CONTINUES through its
///    target. Ownership has already proved that no untrusted account could have
///    placed or re-pointed it, `lchown` being the only way to move a link's own
///    uid, so a link inside a directory this walk found unswappable is a
///    deliberate configuration rather than an attack.
///
///    This question is asked ABOVE the writability rule because a link's own
///    mode bits are not the permission that rule is about. The kernel ignores
///    them on every access, Linux stores 0o777 on every symlink and macOS 0o755,
///    and what actually decides whether another account can replace a link is
///    its PARENT's mode, which this same pass read one component earlier. Judged
///    on its own mode a link would be refused on Linux and admitted on macOS,
///    which is two predicates rather than one.
///
///    Refusing links outright is not available: this module's own fixtures build
///    under a `$TMPDIR` that is `/var/folders/…` on macOS, reached through
///    `/var` -> `private/var`, and an operator-set `CFGD_RUNTIME_DIR` can sit
///    under a host whose `/var/run` is a link to `/run` (cfgd's own system
///    default is `/run/cfgd`, a real directory there, so the default path does
///    not need the admission and the macOS default, under
///    `$HOME/Library/Application Support`, is link-free). What a link cannot do
///    is escape the judgment: its target's components are walked by these same
///    three rules, so a chain that ends in a directory another account owns is
///    refused where it escapes.
///
///    A relative target resolves against the link's own parent, and a LEADING
///    `..` run is folded off that parent lexically rather than left to the `..`
///    refusal below: every component of that parent was read in this same pass
///    and found not to be a link, so dropping one names the directory the kernel
///    would reach. A `..` anywhere else in a target stays in the path and is
///    refused on the next pass, its prefix being components this walk has not
///    read.
///
///    The LEAF is the one exception, and its caller owns it: a link is admitted
///    at every component this walk merely looks THROUGH, and refused at the one
///    component `ensure_owner_private_dir` goes on to chmod, where a path-based
///    mutation would land on the target instead of on the path as given. This
///    walk still follows a leaf link and judges what it points at, which the
///    caller's blanket refusal does not depend on. What running FIRST buys is
///    the reason that applies: a leaf link whose TARGET escapes these three
///    rules is refused by the rule that names the escape, rather than by a kind
///    check that would tell the operator only that their leaf is a link.
/// 3. WRITABILITY BY OTHERS: group- or other-writable is refused unless the
///    sticky bit is set. `/tmp` is 1777 and is a legitimate runtime root, and the
///    sticky bit is precisely what makes a world-writable directory unswappable:
///    only an entry's own owner, the directory's owner or root may rename or
///    unlink it, so an account that can merely create siblings cannot replace the
///    component this walk just read.
///
/// A `..` component in the path as given is refused outright: every `stat` of a
/// prefix ending in one resolves the links above it, which is exactly what this
/// walk is written not to do, so its verdict would be about paths it never read.
///
/// Every refusal raised after the first pass names the folds that assembled the
/// string being judged, in the order they were applied. The path the operator
/// wrote is the first pass's own, so that pass names no fold; past it the string
/// is one only this walk assembled, and a path commonly crosses more than one
/// link before reaching the component that is refused.
#[cfg(unix)]
fn refuse_swappable_path(dir: &std::path::Path, euid: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let refuse = |message: String| DaemonError::HealthSocketError { message };
    let mut path = crate::absolutize_path(dir);
    // Provenance of the path being judged: empty on the first pass, where the
    // path is the one the operator wrote, and one entry per fold from the second
    // pass on, so a refusal can name what they actually wrote rather than a
    // string only this walk ever assembled.
    //
    // EVERY fold is kept rather than only the latest, because a path is commonly
    // assembled by more than one: on macOS `/var` is a link to `private/var` and
    // every per-user and temporary path crosses it, so the walk has already
    // recomposed the whole path under `/private` before it reaches any link an
    // operator planted. Naming the latest alone hands them a string they never
    // typed beside a link that did not produce its prefix.
    let mut composed_from: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    // The one rendering of that provenance, so the three refusals below cannot
    // word it differently. A chain of one renders as it always has.
    let provenance = |folds: &[(std::path::PathBuf, std::path::PathBuf)]| -> String {
        if folds.is_empty() {
            return String::new();
        }
        let links = folds
            .iter()
            .map(|(link, target)| format!("the link {} -> {}", link.posix(), target.posix()))
            .collect::<Vec<_>>()
            .join(", then ");
        format!(" (composed from {links})")
    };
    // One pass per link component. The bound's only job is to terminate this
    // walk: it sits above any chain a unix will resolve, so a non-cyclic chain
    // too long for the host is refused by the kernel's own `ELOOP` at the
    // `mkdir` or the chmod, while a genuine CYCLE reaches neither mutation (this
    // walk runs before both and follows nothing itself) and is what this counter
    // terminates, at the refusal below it. It deliberately cites no figure,
    // because the limit has no single value to cite: on one host the kernel's
    // resolution cap, glibc's `MAXSYMLINKS` and POSIX's `SYMLOOP_MAX` answer
    // three different things, and a number written here would be a fact nobody
    // maintains.
    for _ in 0..64 {
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(refuse(format!(
                "refusing to bind: path {} contains a `..` component, which no \
                 component-by-component check can judge{}",
                path.posix(),
                provenance(&composed_from)
            ))
            .into());
        }
        let mut follow: Option<std::path::PathBuf> = None;
        let mut followed: Option<(std::path::PathBuf, std::path::PathBuf)> = None;
        for component in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if component.as_os_str().is_empty() {
                continue;
            }
            let meta = std::fs::symlink_metadata(component)
                .map_err(|e| refuse(format!("stat path component {}: {}", component.posix(), e)))?;
            let uid = meta.uid();
            if uid != euid && uid != 0 {
                return Err(refuse(format!(
                    "refusing to bind: path component {} is owned by uid {} rather than uid {} or root{}",
                    component.posix(),
                    uid,
                    euid,
                    provenance(&composed_from)
                ))
                .into());
            }
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(component)
                    .map_err(|e| refuse(format!("read link {}: {}", component.posix(), e)))?;
                let rest = path
                    .strip_prefix(component)
                    .unwrap_or(std::path::Path::new(""));
                let base = if target.is_absolute() {
                    target.clone()
                } else {
                    let mut base = component
                        .parent()
                        .unwrap_or(std::path::Path::new("/"))
                        .to_path_buf();
                    let mut parts = target.components().peekable();
                    while let Some(part) = parts.peek() {
                        match part {
                            std::path::Component::ParentDir => {
                                base.pop();
                            }
                            std::path::Component::CurDir => {}
                            _ => break,
                        }
                        parts.next();
                    }
                    for part in parts {
                        base.push(part);
                    }
                    base
                };
                followed = Some((component.to_path_buf(), target));
                // `join` on an empty `rest` appends a separator, so the leaf
                // path would be refused under a name carrying a stray `/`.
                follow = Some(if rest.as_os_str().is_empty() {
                    base
                } else {
                    base.join(rest)
                });
                break;
            }
            let mode = meta.permissions().mode() & 0o7777;
            if mode & 0o022 != 0 && mode & 0o1000 == 0 {
                return Err(refuse(format!(
                    "refusing to bind: path component {} is writable by other accounts \
                     (mode {:o}) without the sticky bit that would keep them from \
                     replacing it{}",
                    component.posix(),
                    mode,
                    provenance(&composed_from)
                ))
                .into());
            }
        }
        match follow {
            Some(next) => {
                path = next;
                if let Some(fold) = followed {
                    composed_from.push(fold);
                }
            }
            None => return Ok(()),
        }
    }
    Err(refuse(format!(
        "refusing to bind: path {} resolves through more than 64 symlinks",
        dir.posix()
    ))
    .into())
}

// --- Health Server ---

/// Bind the daemon's IPC socket at `ipc_path` and serve one connection per
/// accept until the listener is dropped.
///
/// The socket's directory is judged and created by [`ensure_owner_private_dir`]
/// before the bind, and the socket itself is set to 0600 after it: the
/// directory answers for who can reach the path, the mode for who can open what
/// is bound there.
#[cfg(unix)]
pub(crate) async fn run_health_server(
    ipc_path: &str,
    state: Arc<Mutex<DaemonState>>,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let ipc_path_buf = std::path::PathBuf::from(ipc_path);

    if let Some(parent) = ipc_path_buf.parent() {
        ensure_owner_private_dir(parent)?;
    }

    // Stale socket from a crashed daemon — `UnixListener::bind` would error
    // with EADDRINUSE. `check_already_running` cleans up the dead-daemon case
    // before this point, but a stale leftover from a kill -9 still slips
    // through; remove it best-effort.
    if ipc_path_buf.exists() {
        let _ = std::fs::remove_file(&ipc_path_buf);
    }

    let listener = UnixListener::bind(ipc_path).map_err(|e| DaemonError::HealthSocketError {
        message: format!("bind {}: {}", ipc_path, e),
    })?;

    // Tighten the freshly-bound socket to 0600 (default Linux umask 0022
    // leaves it 0755 / world-readable). Done immediately after bind so the
    // window where a parallel `nc -U` could succeed is sub-millisecond. The
    // containment is the refusal above, which has just proved that every
    // component of this path is owned by this process (or by root) and writable
    // by nobody else, so no other account can put an entry in it or re-point
    // the path it sits on. A `umask` around the bind would narrow the socket
    // without any chmod, but umask is process-global and this daemon writes
    // files from other threads while the server binds.
    // follow-ok: `open(2)` on a unix socket is ENXIO, so the primitive cannot serve this path.
    std::fs::set_permissions(&ipc_path_buf, std::fs::Permissions::from_mode(0o600)).map_err(
        |e| DaemonError::HealthSocketError {
            message: format!("chmod socket {}: {}", ipc_path, e),
        },
    )?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("accept: {}", e),
            })?;

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_health_connection(stream, state).await {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}

/// The Windows half of [`run_health_server`]: the same routes over a named pipe
/// instead of a unix socket.
///
/// `first_pipe_instance` is what refuses a second daemon the name, so the pipe
/// needs no directory judgment of its own: the namespace rather than a
/// filesystem path decides who may create it.
#[cfg(windows)]
pub(crate) async fn run_health_server(
    ipc_path: &str,
    state: Arc<Mutex<DaemonState>>,
) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(ipc_path)
        .map_err(|e| DaemonError::HealthSocketError {
            message: format!("create pipe {}: {}", ipc_path, e),
        })?;

    loop {
        server
            .connect()
            .await
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("accept pipe: {}", e),
            })?;

        let connected = server;
        server = ServerOptions::new()
            .first_pipe_instance(false)
            .create(ipc_path)
            .map_err(|e| DaemonError::HealthSocketError {
                message: format!("create pipe {}: {}", ipc_path, e),
            })?;

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_health_connection(connected, state).await {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}

/// Answer one IPC request on `stream`, over the minimal HTTP the CLI's client
/// speaks: `/health`, `/status`, `/drift`, and a 404 for anything else.
///
/// Generic over the stream so the unix-socket and named-pipe servers share one
/// body. A request line this cannot parse a path out of is answered as
/// `/health`, that endpoint existing to tell a supervisor the process is
/// alive.
pub(crate) async fn handle_health_connection<S>(
    stream: S,
    state: Arc<Mutex<DaemonState>>,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = tokio::io::BufReader::new(reader);

    // Read the HTTP request line
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    // Parse path from "GET /path HTTP/1.x"
    let path = request_line.split_whitespace().nth(1).unwrap_or("/health");

    // Drain remaining headers
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    // Clone the state snapshot out of the guard and DROP the guard before any
    // `.await` — holding the mutex across writer.write_all/flush would serialize
    // every /health, /status, and /drift connection. The /drift branch also
    // runs blocking sqlite I/O inside spawn_blocking instead of under the guard.
    let (status_code, body) = {
        let (uptime_secs, status_response, store_path_for_drift) = {
            let st = state.lock().await;
            (
                st.started_at.elapsed().as_secs(),
                st.to_response(),
                st.store_path.clone(),
            )
        };

        match path {
            "/health" => {
                let health = serde_json::json!({
                    "status": "ok",
                    "pid": std::process::id(),
                    "uptime_secs": uptime_secs,
                });
                ("200 OK", serde_json::to_string_pretty(&health)?)
            }
            "/status" => ("200 OK", serde_json::to_string_pretty(&status_response)?),
            "/drift" => {
                let drift_events = match store_path_for_drift {
                    // spawn-blocking-ok: closure resolves no home paths (sqlite open on an explicit store path)
                    Some(p) => tokio::task::spawn_blocking(move || {
                        StateStore::open(&p)
                            .and_then(|s| s.unresolved_drift())
                            .unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default(),
                    None => Vec::new(),
                };

                let drift: Vec<serde_json::Value> = drift_events
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "resource_type": d.resource_type,
                            "resource_id": d.resource_id,
                            "expected": d.expected,
                            "actual": d.actual,
                            "timestamp": d.timestamp,
                        })
                    })
                    .collect();

                (
                    "200 OK",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "drift_count": drift.len(),
                        "events": drift,
                    }))?,
                )
            }
            _ => (
                "404 Not Found",
                serde_json::json!({"error": "not found"}).to_string(),
            ),
        }
    };

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_code,
        body.len(),
        body
    );

    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;

    Ok(())
}
// --- Status Query (for cfgd daemon status) ---

/// Connect to the daemon IPC endpoint. Returns `None` if the daemon is not
/// reachable. `runtime_over` carries the `--runtime-dir` override and `scope`
/// the `--scope system` selection so the client resolves the same socket the server
/// bound; pass `None`/[`crate::Scope::User`] for env/default.
pub(crate) fn connect_daemon_ipc(
    runtime_over: Option<&std::path::Path>,
    scope: crate::Scope,
) -> Option<IpcStream> {
    let path = super::resolve_default_ipc_path(runtime_over, scope);
    #[cfg(unix)]
    {
        if !path.exists() {
            return None;
        }
        let stream = StdUnixStream::connect(&path).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        Some(IpcStream::Unix(stream))
    }
    #[cfg(windows)]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .ok()?;
        Some(IpcStream::Pipe(file))
    }
}

/// Platform-specific IPC stream wrapper implementing Read + Write.
pub(crate) enum IpcStream {
    #[cfg(unix)]
    Unix(StdUnixStream),
    #[cfg(windows)]
    Pipe(std::fs::File),
}

impl std::io::Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.read(buf),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.read(buf),
        }
    }
}

impl std::io::Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.write(buf),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => s.flush(),
            #[cfg(windows)]
            IpcStream::Pipe(f) => f.flush(),
        }
    }
}

/// Query the running daemon's status over IPC. `runtime_over` carries the
/// `--runtime-dir` override and `scope` the `--scope system` selection so the socket
/// is resolved identically to the server's bind; pass `None`/[`crate::Scope::User`]
/// for env/default.
pub fn query_daemon_status(
    runtime_over: Option<&std::path::Path>,
    scope: crate::Scope,
) -> Result<Option<DaemonStatusResponse>> {
    let mut stream = match connect_daemon_ipc(runtime_over, scope) {
        Some(s) => s,
        None => return Ok(None),
    };

    write!(
        stream,
        "GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| DaemonError::HealthSocketError {
        message: format!("write request: {}", e),
    })?;

    // Cap total bytes read from the daemon so a hijacked or hostile peer
    // can't stream multi-GiB garbage and OOM the CLI. The `Take` wrapper
    // returns Ok(0) once `MAX_RESPONSE_BYTES` are consumed, which BufRead
    // reports as a clean EOF — the underlying `limit()` then distinguishes
    // "real EOF" from "cap reached".
    let mut limited = std::io::Read::take(&mut stream, MAX_RESPONSE_BYTES);
    let reader = BufReader::new(&mut limited);
    let mut lines: Vec<String> = Vec::new();
    let mut in_body = false;

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| DaemonError::HealthSocketError {
            message: format!("read response: {}", e),
        })?;

        if in_body {
            lines.push(line);
        } else if line.trim().is_empty() {
            in_body = true;
        }
    }

    // `Take::limit()` is the remaining unread budget; zero means the cap was hit
    // before the peer closed the socket, i.e. the response was truncated.
    if limited.limit() == 0 {
        return Err(DaemonError::HealthSocketError {
            message: format!("daemon response exceeded {} bytes", MAX_RESPONSE_BYTES),
        }
        .into());
    }

    let body = lines.join("\n");
    if body.is_empty() {
        return Ok(None);
    }

    let status: DaemonStatusResponse =
        serde_json::from_str(&body).map_err(|e| DaemonError::HealthSocketError {
            message: format!("parse response: {}", e),
        })?;

    Ok(Some(status))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ensure_owner_private_dir_creates_with_mode_700() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc");
        ensure_owner_private_dir(&dir).expect("should create dir owner-private");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "must enforce 0700 on the IPC parent");
    }

    #[test]
    fn ensure_owner_private_dir_idempotent_when_already_compliant() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        ensure_owner_private_dir(&dir).expect("idempotent on already-compliant dir");
    }

    #[test]
    fn ensure_owner_private_dir_refuses_a_regular_file_standing_in_for_the_directory() {
        // A regular file standing where the socket's directory belongs is the
        // leaf, so the kind check settles it before anything is created or
        // chmodded: the no-follow chmod would open a regular file happily and
        // rewrite its mode instead of refusing it.
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("not-a-dir");
        std::fs::write(&file_path, "hello").unwrap();
        let err = ensure_owner_private_dir(&file_path)
            .expect_err("a regular file standing in for the socket directory must be refused");
        let msg = err.to_string();
        // unfolded-path-ok: the kind refusal is worded by `ensure_owner_private_dir` against the path it was handed, which the ancestor walk's folds never reach.
        assert!(
            msg.contains("is not a directory") && msg.contains(&file_path.display().to_string()),
            "the refusal must name the path and its kind, got: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // run_health_server — end-to-end over a REAL Unix socket
    //
    // The existing daemon tests drive `handle_health_connection` directly via an
    // in-memory duplex, which never exercises the server's socket bind, the
    // post-bind 0600 chmod, the accept loop, or the per-connection spawn. This
    // test boots the actual server on a temp socket and round-trips a request
    // through it, asserting both the wire response and the on-disk socket mode.
    // ---------------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_health_server_binds_socket_0600_and_serves_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let tmp = tempfile::tempdir().unwrap();
        // Nest the socket under a subdir so ensure_owner_private_dir creates +
        // hardens the parent (covers the parent-setup path inside the server).
        let sock = tmp.path().join("rundir").join("cfgd.sock");
        let sock_str = sock.to_string_lossy().into_owned();

        let state = Arc::new(Mutex::new(DaemonState::new()));
        let server_state = Arc::clone(&state);
        let server = tokio::spawn(async move { run_health_server(&sock_str, server_state).await });

        // Wait for the socket to appear AND for the post-bind chmod to land:
        // existence alone proves only the bind, and reading the mode right
        // after can race into the bind-to-chmod window (harmless in
        // production — the 0700 parent already blocks other users — but the
        // assert would read the pre-chmod 0755).
        let socket_mode = |p: &std::path::Path| {
            std::fs::metadata(p)
                .ok()
                .map(|m| m.permissions().mode() & 0o777)
        };
        let mut waited = 0;
        while socket_mode(&sock) != Some(0o600) && waited < 200 {
            // sleep-ok: bounded poll on a filesystem permission side effect, not a fixed-duration guess
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waited += 1;
        }
        assert!(sock.exists(), "server must bind the socket file");

        // The freshly-bound socket must be owner-only (0600).
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "server must tighten the socket to 0600");
        // And the parent dir must be owner-private (0700).
        let parent_mode = std::fs::metadata(sock.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "parent dir must be 0700");

        // Round-trip a /status request through the real accept loop.
        let mut client = UnixStream::connect(&sock).await.unwrap();
        client
            .write_all(b"GET /status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        // Half-close the write side to signal end-of-request. The server keys
        // off the blank-line header terminator (not EOF), so the response is
        // produced regardless. On macOS `shutdown()` can race the connection
        // setup and return ENOTCONN even though the request was already
        // flushed; tolerate that — the subsequent read still gets the response.
        match client.shutdown().await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {}
            Err(e) => panic!("unexpected shutdown error: {e}"),
        }

        let mut raw = String::new();
        client.read_to_string(&mut raw).await.unwrap();

        assert!(
            raw.starts_with("HTTP/1.1 200 OK\r\n"),
            "status line, got: {}",
            &raw[..raw.len().min(40)]
        );
        let (_head, body) = raw.split_once("\r\n\r\n").expect("header/body split");
        let json: serde_json::Value = serde_json::from_str(body).expect("body is JSON");
        // Default DaemonState::new() values on the camelCase wire.
        assert_eq!(json["running"], true);
        assert_eq!(json["driftCount"], 0);
        assert_eq!(json["sources"][0]["name"], "local");

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_health_server_removes_stale_socket_before_bind() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Pre-create a stale leftover file at the socket path (simulating a
        // kill -9 leftover). The server must remove it and bind cleanly.
        std::fs::write(&sock, b"stale").unwrap();
        assert!(sock.exists());

        let sock_str = sock.to_string_lossy().into_owned();
        let state = Arc::new(Mutex::new(DaemonState::new()));
        let server = tokio::spawn(async move { run_health_server(&sock_str, state).await });

        // Connect with a short retry: a successful /health round-trip proves the
        // stale file was removed and a fresh listener bound in its place.
        let mut got_response = None;
        for _ in 0..50 {
            // sleep-ok: bounded retry loop on the socket's own connect result, not a fixed-duration guess
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let Ok(mut client) = UnixStream::connect(&sock).await else {
                continue;
            };
            if client
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .is_err()
            {
                continue;
            }
            // FreeBSD reports ENOTCONN here when the server has already
            // answered and closed the connection; the response is still
            // buffered, so let the read decide instead of unwrapping.
            let _ = client.shutdown().await;
            let mut raw = String::new();
            if client.read_to_string(&mut raw).await.is_ok() && !raw.is_empty() {
                got_response = Some(raw);
                break;
            }
        }

        let raw = got_response.expect("server must serve after clearing the stale socket");
        assert!(raw.starts_with("HTTP/1.1 200 OK"));
        let (_head, body) = raw.split_once("\r\n\r\n").unwrap();
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["status"], "ok");

        server.abort();
    }

    // ---------------------------------------------------------------------------
    // query_daemon_status — client-side parsing over a REAL Unix socket
    //
    // The async server tests above only drive `handle_health_connection`. The
    // synchronous client path (`connect_daemon_ipc` → `IpcStream` Read/Write →
    // header/body split → JSON parse → MAX_RESPONSE_BYTES cap) is exercised here
    // by standing up a hand-rolled fake server on a temp socket that returns a
    // crafted HTTP response, then pointing the client at it via the
    // `CFGD_DAEMON_IPC_PATH` override so `resolve_default_ipc_path` resolves to
    // exactly our socket.
    // ---------------------------------------------------------------------------

    /// Spawn a one-shot fake daemon at `sock_path`. The accepted connection's
    /// request is drained, then `raw_response` is written verbatim and the
    /// socket is closed. Returns the join handle so the test can await teardown.
    fn spawn_fake_daemon(
        sock_path: std::path::PathBuf,
        raw_response: &'static [u8],
    ) -> std::thread::JoinHandle<()> {
        use std::io::{Read as _, Write as _};
        let listener = std::os::unix::net::UnixListener::bind(&sock_path)
            .expect("fake daemon must bind temp socket");
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                // Drain the request line + headers (until blank line) so the
                // client's write side completes before responding.
                let mut buf = [0u8; 1024];
                // A single read is enough: the client's request is well under 1 KiB
                // and not every byte is needed — only the kernel buffer needs to be
                // drained enough that our write isn't blocked.
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(raw_response);
                let _ = conn.flush();
            }
        })
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_parses_real_response_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Crafted /status response: a full HTTP frame whose camelCase body
        // carries distinct, non-default field values, so the test fails if any
        // field is dropped or mis-mapped (uptimeSecs→uptime_secs, driftCount→
        // drift_count) during deserialization.
        const RESP: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"running\":true,\"pid\":4242,\"uptimeSecs\":99,\"lastReconcile\":\"2026-06-13T00:00:00Z\",\"lastSync\":\"2026-06-13T01:00:00Z\",\"driftCount\":7,\"sources\":[{\"name\":\"remote\",\"lastSync\":null,\"lastReconcile\":null,\"driftCount\":2,\"status\":\"degraded\"}]}";
        let handle = spawn_fake_daemon(sock.clone(), RESP);

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let status = query_daemon_status(None, crate::Scope::User)
            .expect("client must parse a well-formed daemon response")
            .expect("a non-empty body must deserialize to Some(status)");

        handle.join().unwrap();

        assert!(status.running, "running must round-trip true");
        assert_eq!(status.pid, 4242, "pid must round-trip exactly");
        assert_eq!(status.uptime_secs, 99, "uptimeSecs → uptime_secs mapping");
        assert_eq!(
            status.last_reconcile.as_deref(),
            Some("2026-06-13T00:00:00Z")
        );
        assert_eq!(status.last_sync.as_deref(), Some("2026-06-13T01:00:00Z"));
        assert_eq!(status.drift_count, 7, "driftCount → drift_count mapping");
        assert_eq!(status.sources.len(), 1);
        assert_eq!(status.sources[0].name, "remote");
        assert_eq!(status.sources[0].drift_count, Some(2));
        assert_eq!(status.sources[0].status, "degraded");
        assert!(status.update_available.is_none(), "absent field → None");
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_empty_body_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Valid HTTP framing but a zero-length body after the blank line.
        const RESP: &[u8] =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";
        let handle = spawn_fake_daemon(sock.clone(), RESP);

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let result = query_daemon_status(None, crate::Scope::User)
            .expect("an empty body is a clean Ok(None), not an error");
        handle.join().unwrap();
        assert!(result.is_none(), "empty body must map to Ok(None)");
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_unreachable_socket_returns_none() {
        // Point the client at a socket path that was never bound. connect_daemon_ipc
        // sees the path does not exist and returns None → Ok(None), not an error.
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("never-bound.sock");
        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let result = query_daemon_status(None, crate::Scope::User)
            .expect("an unreachable daemon is not an error");
        assert!(result.is_none(), "no socket file → Ok(None)");
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_malformed_json_body_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // Well-framed HTTP, but the body is not valid JSON for DaemonStatusResponse.
        const RESP: &[u8] = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{not valid json at all";
        let handle = spawn_fake_daemon(sock.clone(), RESP);

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let err = query_daemon_status(None, crate::Scope::User)
            .expect_err("a malformed body must surface a HealthSocketError");
        handle.join().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("parse response"),
            "malformed JSON must map to the parse-response HealthSocketError, got: {msg}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn query_daemon_status_oversize_response_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("cfgd.sock");
        // A response whose body alone exceeds MAX_RESPONSE_BYTES. The Take cap
        // trips before EOF, so query_daemon_status must reject it rather than
        // attempt to buffer/parse multi-hundred-KiB of attacker-controlled data.
        // Build it once at module load so the &'static lifetime fits spawn_fake_daemon.
        static OVERSIZE: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
            let header = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
            let mut v = Vec::with_capacity(header.len() + MAX_RESPONSE_BYTES as usize + 1024);
            v.extend_from_slice(header);
            // One giant JSON-ish line well past the cap.
            v.extend_from_slice(b"{\"junk\":\"");
            v.resize(v.len() + MAX_RESPONSE_BYTES as usize + 512, b'A');
            v.extend_from_slice(b"\"}");
            v
        });
        let listener = std::os::unix::net::UnixListener::bind(&sock)
            .expect("fake daemon must bind temp socket");
        let handle = std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                // Best-effort: the client stops reading at the cap and closes, so
                // a broken pipe partway through the write is expected and fine.
                let _ = conn.write_all(&OVERSIZE);
                let _ = conn.flush();
            }
        });

        let _guard =
            crate::test_helpers::EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", sock.to_str().unwrap());
        let err = query_daemon_status(None, crate::Scope::User)
            .expect_err("an over-cap response must be rejected, not parsed");
        let _ = handle.join();
        let msg = err.to_string();
        assert!(
            msg.contains("exceeded") && msg.contains(&MAX_RESPONSE_BYTES.to_string()),
            "oversize response must cite the byte cap, got: {msg}"
        );
    }

    /// The `..` refusal names the link that composed the path, and the composed
    /// path carries no separator the operator never wrote.
    ///
    /// A `..` that is not part of a target's LEADING run stays in the path and is
    /// refused on the next pass, by which point the path being judged is one this
    /// walk assembled: without the provenance clause an operator reads a string
    /// they never typed and cannot find the link that produced it. The first pass
    /// judges the path as given, so it carries no clause. `join` on an empty
    /// remainder would append a separator, so the composed path is also asserted
    /// whole rather than by a `contains` of its prefix.
    ///
    /// The fixture is rooted on [`crate::test_helpers::folded_temp_root`]: the
    /// walk folds every link above the planted one first, and on a host whose
    /// `$TMPDIR` is reached through one there is such a link, so an expectation
    /// built from the tempdir's own path names a prefix the walk has already
    /// recomposed. Folding the root also makes the planted link the ONE fold on
    /// every host, which is what this pin's single-clause wording is about.
    #[test]
    fn the_parent_dir_refusal_names_the_link_that_composed_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::test_helpers::folded_temp_root(tmp.path());
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::create_dir(nested.join("real")).unwrap();
        let via = nested.join("via");
        // A `..` behind a Normal component, which the leading-run fold leaves in
        // place by design.
        std::os::unix::fs::symlink("real/../real", &via).unwrap();

        let err = ensure_owner_private_dir(&via)
            .expect_err("a target carrying a non-leading `..` must be refused");
        let msg = format!("{err}");
        let composed = nested.join("real").join("..").join("real");
        assert!(
            msg.contains(&format!("path {} contains", composed.display())),
            "the refusal must name the composed path exactly, got {msg}"
        );
        assert!(
            msg.contains(&format!(
                "(composed from the link {} -> real/../real)",
                via.display()
            )),
            "the refusal must name the link and its target, got {msg}"
        );

        // The first pass judges the operator's own path, so it states no
        // provenance it does not have.
        let as_given = nested.join("..").join("nested");
        let err = ensure_owner_private_dir(&as_given)
            .expect_err("a `..` in the path as given must be refused");
        let msg = format!("{err}");
        assert!(
            msg.contains("contains a `..` component") && !msg.contains("composed from"),
            "a first-pass refusal must carry no provenance clause, got {msg}"
        );
    }

    /// A link target's LEADING `.`/`..` run is folded against the link's own
    /// parent, and the `..` refusal on the next pass is what makes the folded
    /// path observable.
    ///
    /// The fold decides what this WALK judges and nothing else: where a
    /// directory is created is the kernel resolving the same path, so a fixture
    /// that reads the filesystem back cannot tell two folds apart. Each target
    /// here therefore carries a trailing `nope/..` behind its leading run. The
    /// `..` survives the fold by design, is refused on the next pass, and that
    /// refusal names the composed path byte for byte.
    ///
    /// Rooted on [`crate::test_helpers::folded_temp_root`] for the reason its
    /// sibling `the_parent_dir_refusal_names_the_link_that_composed_the_path`
    /// states: the expectation is a path this walk composed, so it is built from
    /// the root the walk judges rather than the one the tempdir reports.
    #[test]
    fn a_leading_dot_and_dot_dot_run_folds_against_the_links_own_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::test_helpers::folded_temp_root(tmp.path());
        let nested = root.join("nested");
        let deep = nested.join("deep");
        std::fs::create_dir_all(&deep).unwrap();

        // `.` is consumed without moving the base, so the composed path hangs
        // off the link's own parent.
        let here = deep.join("here");
        std::os::unix::fs::symlink("./nope/..", &here).unwrap();
        let err =
            ensure_owner_private_dir(&here).expect_err("a target carrying `..` must be refused");
        let composed = deep.join("nope").join("..");
        assert!(
            format!("{err}").contains(&format!("path {} contains", composed.display())),
            "a `.` target must fold to the link's own parent, got {err}"
        );

        // `..` pops exactly one component off that parent.
        let up = deep.join("up");
        std::os::unix::fs::symlink("../nope/..", &up).unwrap();
        let err =
            ensure_owner_private_dir(&up).expect_err("a target carrying `..` must be refused");
        let composed = nested.join("nope").join("..");
        assert!(
            format!("{err}").contains(&format!("path {} contains", composed.display())),
            "a `..` target must fold one level above the link's parent, got {err}"
        );
    }

    /// A leading `..` run longer than the link's parent is deep saturates at `/`
    /// rather than walking off the top.
    ///
    /// `PathBuf::pop` returns false at the root and leaves the path alone, which
    /// is the kernel's own `..`-at-root behaviour. The target carries more `..`
    /// components than the fixture has path components, and the refusal names
    /// the composed path, so the saturation is read off the message rather than
    /// assumed from std's documentation.
    #[test]
    fn a_leading_parent_run_past_the_root_saturates_at_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("nested").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        let way_up = deep.join("way-up");
        std::os::unix::fs::symlink("../../../../../../../../../nope/..", &way_up).unwrap();

        let err =
            ensure_owner_private_dir(&way_up).expect_err("a target carrying `..` must be refused");
        assert!(
            format!("{err}").contains("path /nope/.. contains"),
            "the pops must saturate at the root, got {err}"
        );
    }

    /// A refusal reached after several folds names every link that contributed
    /// to the string being judged, in the order they were applied.
    ///
    /// One fold per refusal is what a path crossing a single link needs, and it
    /// is not the common shape: on macOS `/var` is a link to `private/var` and
    /// every per-user and temporary path crosses it, so the walk has recomposed
    /// the whole path under `/private` before it reaches anything an operator
    /// planted. Keeping only the latest fold shows them a prefix they never typed
    /// beside a link that did not produce it, which is the exact gap the clause
    /// exists to close.
    ///
    /// Two links rather than one, the second carrying a non-leading `..` so the
    /// third pass refuses and the assembled string is observable. The first
    /// target is relative, so the clause is also proven to render a target as the
    /// link spells it rather than as the walk resolved it.
    #[test]
    fn a_refusal_after_several_folds_names_every_link_that_composed_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::test_helpers::folded_temp_root(tmp.path());
        std::fs::create_dir(root.join("one")).unwrap();
        std::os::unix::fs::symlink("one", root.join("first")).unwrap();
        std::os::unix::fs::symlink("real/../real", root.join("one").join("second")).unwrap();

        let err = ensure_owner_private_dir(&root.join("first").join("second"))
            .expect_err("a target carrying a non-leading `..` must be refused");
        let msg = format!("{err}");
        let composed = root.join("one").join("real").join("..").join("real");
        assert!(
            msg.contains(&format!("path {} contains", composed.display())),
            "the refusal must name the path both folds assembled, got {msg}"
        );
        assert!(
            msg.contains(&format!(
                "(composed from the link {} -> one, then the link {} -> real/../real)",
                root.join("first").display(),
                root.join("one").join("second").display()
            )),
            "the refusal must name both folds in the order they were applied, got {msg}"
        );
    }

    /// A refusal about a path COMPONENT names the fold that put that component
    /// in the path, not the `..` refusal alone.
    ///
    /// All three refusals this walk raises past its first pass judge a string it
    /// assembled, and all three read the one provenance composer. Without the
    /// clause an operator is told a directory of theirs is world-writable under a
    /// name they never wrote: the path asked about here is `<root>/via/cfgd`, and
    /// what the refusal names is `<root>/open`.
    ///
    /// The writability rule is the one of the three a test can arrange at any
    /// uid. Its sibling ownership rule needs the power to `chown`, and reads the
    /// same composer.
    #[test]
    fn a_refusal_on_a_component_the_walk_folded_to_names_the_link_that_composed_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::test_helpers::folded_temp_root(tmp.path());
        let open = root.join("open");
        std::fs::create_dir(&open).unwrap();
        // Group- and other-writable without the sticky bit is the one shape the
        // walk's writability rule refuses.
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::os::unix::fs::symlink("open", root.join("via")).unwrap();

        let err = ensure_owner_private_dir(&root.join("via").join("cfgd"))
            .expect_err("a component other accounts can write must be refused");
        let msg = format!("{err}");
        assert!(
            msg.contains(&format!("path component {} is writable", open.display())),
            "the refusal must name the component the fold reached, got {msg}"
        );
        assert!(
            msg.contains(&format!(
                "(composed from the link {} -> open)",
                root.join("via").display()
            )),
            "the refusal must name the fold that put that component in the path, got {msg}"
        );
    }

    /// The explicit 0700 survives a umask that withholds the owner bits, which
    /// is the whole reason the chmod stands beside the `mkdir`'s mode argument.
    ///
    /// `mkdir(2)` masks its mode with the process umask, so under `0o377` the
    /// directory would be created `0o400`: owner-private enough to pass both of
    /// this function's own refusals, and not traversable by the daemon that
    /// needs to bind in it. The chmod is `fchmod(2)`, which no umask touches.
    ///
    /// Run in a CHILD, re-entered through this same test by name. A umask is
    /// process-global and is NOT process environment, so neither the env-mutator
    /// walk nor `SERIAL_PINS` governs it and `#[serial_test::serial]` would not
    /// help: that attribute orders a test against other attributed tests, while
    /// a raised umask changes the default mode of every file the thousands of
    /// unattributed tests create in parallel, several of which assert a mode.
    /// A child's umask is its own, and the directory it leaves behind on the
    /// shared filesystem is what this process asserts against.
    ///
    /// The residual no shape closes: libtest offers no re-entry channel that is
    /// not ambient, so the child can only be selected by environment and a
    /// process inheriting one by accident takes the branch. What the two
    /// variables and the restored mask buy is that such an entry is LOUD rather
    /// than a vacuous pass, and that the raise never outlives the one `mkdir`.
    #[test]
    fn the_leaf_chmod_survives_a_umask_that_withholds_the_owner_bits() {
        const TEST_PATH: &str = "daemon::health_ipc::tests::the_leaf_chmod_survives_a_umask_that_withholds_the_owner_bits";
        const DIR_VAR: &str = "CFGD_UMASK_PIN_DIR";
        const TEST_VAR: &str = "CFGD_UMASK_PIN_TEST";

        if let Ok(dir) = std::env::var(DIR_VAR) {
            // Two variables rather than one, because this branch raises the
            // process umask and a shell exporting a single familiar name would
            // have the PARENT do that to every test creating a file beside it.
            // The second carries this test's own path, which nothing but the
            // parent's `Command::env` below writes, and a mismatch PANICS: an
            // early return would leave the pin green having asserted nothing.
            assert_eq!(
                std::env::var(TEST_VAR).ok().as_deref(),
                Some(TEST_PATH),
                "{DIR_VAR} is set without a matching {TEST_VAR}, so this process was entered \
                 as the umask child by something other than its own parent"
            );
            // `umask(2)` answers with the mask it replaced, so the raise is
            // scoped to the one `mkdir` below rather than to the rest of the
            // process: it is handed back on the next statement.
            let prior = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o377));
            let created = ensure_owner_private_dir(std::path::Path::new(&dir));
            nix::sys::stat::umask(prior);
            created.expect("the child must create the socket directory under the raised umask");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ipc-umask");
        let exe = std::env::current_exe().expect("the test binary is a real file");
        let out = std::process::Command::new(exe)
            .args(["--exact", TEST_PATH, "--nocapture", "--test-threads", "1"])
            // Per-child `env` rather than a process mutation: the parent's own
            // environment is never touched.
            .env(DIR_VAR, &dir)
            .env(TEST_VAR, TEST_PATH)
            .output()
            .expect("the test binary must be re-runnable as a child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "the child run must pass; stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // `--exact` against a renamed test matches nothing and still exits 0, so
        // the count is asserted rather than the status alone.
        assert!(
            stdout.contains("1 passed"),
            "the child must have run exactly this test; stdout: {stdout}"
        );

        let mode = std::fs::metadata(&dir)
            .expect("the child must have created the directory")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o700,
            "the explicit chmod must restore what the umask withheld, got {mode:o}"
        );
    }
}
