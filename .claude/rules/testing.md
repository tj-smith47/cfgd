---
paths: ["crates/**/*.rs"]
---
# cfgd Testing

- `cargo test` must pass before any phase is considered complete.
- Unit tests for pure logic (config parsing, diffing, template rendering). Co-located in `#[cfg(test)] mod tests {}` within each module.
- Integration tests in `tests/`. Every one that runs the real binary spawns it through `cfgd_binary::cfgd_bin()` (`crates/cfgd/tests/cfgd_binary/mod.rs`), which opts out of the update check, removes every inherited `CFGD_*` and systemd `*_DIRECTORY` variable, and points every variable in `cfgd_binary/isolated_env.rs` and the working directory into the test's own temp dir. No test spells `cargo_bin(`, `cargo_bin!(`, `cargo_bin_cmd!(`, `Command::new("cfgd"` or `CARGO_BIN_EXE_cfgd` itself; `every_integration_test_spawns_the_binary_through_the_one_isolating_constructor` fails until it calls the constructor.
- Package manager tests use mock trait implementations and make no real system calls.
- Use `tempfile` for any test that touches the filesystem.

## The reconciler cannot resolve a real `$HOME` under test

`Reconciler::new` resolves the home directory once, in `resolved_home()`. The
real-home arm is gated on `cfg(not(any(test, feature = "test-helpers")))` — the
feature matters, because `cfg(test)` is set only while compiling cfgd-core's
own test binary. Without it a `cfgd` / `cfgd-operator` / `cfgd-csi` test links a
release-shaped core, and a non-dry-run apply of a profile carrying `spec.env`
rewrites the operator's own `~/.cfgd.env`, `~/.config/environment.d/cfgd.conf`
and shell rc files. Each consumer enables `test-helpers` in
`[dev-dependencies]` only, and the one crate that needs it as a normal
dependency (`cfgd-test-fixtures`, `publish = false`) stays out of the
workspace `default-members`, so no shipped binary compiles the test arm: a
root-level `cargo build` resolves features over the default members alone.
`the_default_members_are_every_member_but_the_unpublished_ones` holds that
list to `members` minus every `publish = false` member.

A test that installs `with_test_home_guard` gets the home it asked for. A test
that installs nothing gets a throwaway directory unique to its own thread,
named `cfgd-unguarded-test-home-<pid>-<n>` under the system temp dir. That
directory is only named and never created: anything appearing there is a test that writes
env surfaces and should install a guard.

That fallback is the RECONCILER's alone. The env CHECK
(`env_verify_results`, and the `~/`-fold every report path passes through)
resolves `~` with `expand_tilde`, which has no unguarded-test arm and reads the
real `$HOME`: an in-process test that declares `spec.env`/`spec.aliases` and
runs a `cmd_*` reports the invoking user's own env surface. That passes on a
development box, which dogfoods cfgd and already holds every planned target,
and fails on a CI runner whose `$HOME` holds none — so such a test installs
`with_test_home_guard` and plants its surface through
`MergedEnvItems::managed_env_files` / `managed_env_source_lines`.
`every_in_process_test_declaring_shell_items_holds_a_test_home` walks every
crate's `tests/`.

Blocking dispatch loses the thread-local, so any closure that may resolve `~`
goes through `cfgd_core::spawn_blocking_with_test_home`. `audit.sh` rejects a
raw `tokio::task::spawn_blocking` anywhere in workspace production code unless
the call line (or the line above it) carries
`// spawn-blocking-ok: <why the closure resolves no home paths>`.

## No test run reaches the real config directory

The shell suites under `tests/e2e/*/scripts/` run the real binary as the
invoking user, so every path cfgd resolves from `$HOME` or `$XDG_*` is that
user's own. `tests/e2e/common/scratch-home.sh` is the ONE redirect: it exports
`HOME`, `USERPROFILE` and all four `XDG_*` directories into the run's scratch
root, asserts the redirect took before anything else reads `$HOME`, and passes
through only the seams a suite genuinely needs out of the real home
(`CARGO_HOME`, `RUSTUP_HOME`, `KUBECONFIG`, `DOCKER_CONFIG`, the three
`HELM_*`). It also fingerprints the real config directory at source time;
`assert_real_config_dir_unchanged` re-reads it and every `run-all.sh` fails the
whole run when it moved. `helpers.sh` sources it, so every suite reaches it, and
`every_e2e_suite_runs_under_the_one_scratch_home` (`crates/cfgd/src/cli/tests.rs`)
walks every `tests/e2e/*/scripts/run-all.sh` for both halves, so a new suite
directory trips over the rule.

The Rust integration tests under `crates/cfgd/tests/` have the same need and
one constructor for it: `cfgd_binary::cfgd_bin()` removes every `CFGD_*` the
test process inherited (matched case-insensitively, as Windows names them) and
the four systemd `*_DIRECTORY` variables `REMOVED_ENV` lists, then builds every
real-binary command with `HOME`, `USERPROFILE`, the four `XDG_*_HOME`
directories, `XDG_RUNTIME_DIR`, `LOCALAPPDATA`, `CFGD_STATE_DIR`,
`CFGD_CACHE_DIR` and `CFGD_RUNTIME_DIR` (the list in
`cfgd_binary/isolated_env.rs`) pointed into a temp dir owned by the
calling test's thread, and starts it in a working directory there too, so no
spawn reads the developer's config, writes project scope into the checkout,
takes a lock in the real runtime dir, or shares another test's `state.db`. The
`CFGD_*` overrides are what isolate Windows, whose cache and runtime
known-folder lookups ignore the environment. `CFGD_CONFIG_DIR` stays unset: the
CLI reads it as an explicit `--config-dir`, and `XDG_CONFIG_HOME` already moves
the default config directory on every OS. A test that needs its own home, `CFGD_*` value or working
directory sets it on the returned command, and its setting wins; one that sets
`XDG_CACHE_HOME` to exercise cache resolution also removes `CFGD_CACHE_DIR`,
which outranks it.

An in-process test parses an argv through `cli::HermeticParse`
(`try_parse_hermetic`, or `try_parse_reading_env` for a variable the test sets
itself). Clap's own `Parser` methods are off limits: they read every `env =` binding
from the exported environment; `every_in_process_parse_goes_through_the_hermetic_parser`
walks both `src` and `tests`.

The binary answers for its own half: a verb that materialises a config from
`--from` refuses to write into a default config directory that already holds a
`cfgd.yaml` or `cfgd.toml`, is not empty, or is a symlink
(`crates/cfgd/tests/from_default_dir_refusal.rs`). The question is asked about
the DIRECTORY — `cfgd_core::names_the_same_path` against `default_config_dir()`,
which folds both spellings lexically first and then asks the inode question for
what the fold cannot see — so a `--config` that walks back into it through `..`
or names what it is a symlink to is refused under the same rule, and every
`--from` verb reads its destination
through `init::from_destination`
(`every_from_verb_takes_its_destination_from_from_destination`). Both halves exist because
neither one was enough: an e2e `apply --from` pointed at a scratch `--config`
cloned its fixture into a developer's `~/.config/cfgd`, and `secret` wrote an
age key beside it.

## An e2e drift case moves only a key its own pod owns

The node and full-stack e2e jobs run at the same time, and their privileged
test pods can land on the same worker. A host-global sysctl such as
`vm.max_map_count` is one value for every pod on that node, so a drift written
by one job can be restored by the other between a case's two reads, and the
case fails for a reason that has nothing to do with cfgd. Every `sysctl -w`
under `tests/e2e/` that introduces or restores drift writes
`net.ipv4.ip_forward` (declared `"1"` by every profile a drift case runs
against; `net.*` lives in the pod's own network namespace because no test pod
sets `hostNetwork`). The one host-global write allowed is the `fs.inotify.*`
raise ahead of a daemon start, which every job sets to the same value and no
case reads back. A compliance before/after case goes through
`sysctl_drift_case` in `tests/e2e/common/helpers.sh`, which fails the case
outright for any key outside `net.*`: a drift case reads its key back, so the
`fs.inotify.*` exemption does not reach it. The audit gate "e2e sysctl writes
stay pod-private" (`e2e_sysctl_write_scan` in `.claude/scripts/audit.sh`) reads
every file under `tests/e2e/`, joins backslash-continued lines, and judges each
written key on its own, in four forms: every `key=` token after a `sysctl`
whose flags include `-w` (alone or combined, as `-q -w` or `-wq`) or `--write`,
up to the end of that command; a redirect (`>`, `>>` or `>|`) into
`/proc/sys/<path>`; every `/proc/sys/<path>` operand of a `tee`, whatever
options (`-a`, `--append`) or other operands come with it; and the key argument
of each `sysctl_drift_case` call. Paths are read as dotted keys. Only the bare
`sysctl` form may write `fs.inotify.*`; every other form takes `net.*` alone.
Any other key is an error naming the file, line and key. A key held in a shell
variable is exempt only as the `$key` of a `sysctl` write inside the
`sysctl_drift_case()` body, whose callers are judged instead; any other
variable key, in the body or anywhere else, is an "unresolvable key" error. The
repository run also fails when the scan cannot read `tests/e2e/`, finds no file
there, or exempts no `fs.inotify` raise, so a scan that stops seeing the daemon
starts cannot report clean. The `bad_e2e_sysctl_*` and `good_e2e_sysctl_*`
fixtures under `.claude/scripts/audit-tests/` hold one case per form for
`task audit:test`.

## An e2e port-forward goes through `port_forward`

Every `kubectl port-forward` under `tests/e2e/` is started by `port_forward`
in `tests/e2e/common/helpers.sh` (target `svc/<name>` or `pod/<name>`) and
ended by `stop_port_forward`. The helper returns the PID only once the local
port accepts a connection (`E2E_PORT_FORWARD_TRIES` half-second probes, a
positive integer, 30 by default), and on a timeout or an early kubectl exit prints kubectl's own output
and returns 1; a fixed `sleep` and a discarded stderr are what left a metrics
case failing with nothing to diagnose. `stop_port_forward` sends SIGKILL to a
kubectl still running 5s after SIGTERM. A case that scrapes an endpoint and
asserts on the body fetches it with `http_get_to_file` and puts
`http_evidence` (status, content type, line count, first 15 lines, and curl's
exit code and error when nothing answered) in every fail reason; a counter
check reads the body through `metric_sample_lines <family> <body>` or
`metric_sample_value <family> <labels> <body>` (absent reads 0), which match
the sample line only (never `# HELP`/`# TYPE`) and only under `<family>_total`,
the one name the PR-built components render (a doubled suffix is not read);
`tests/e2e/common/test-metrics.sh` fails on a counter sample matched by hand
anywhere under `tests/e2e/`. A case that needs several words of a space-joined
list (a jsonpath `{range}` of conditions, `availablePlatforms[*]`) calls
`has_all_words <list> <word>...`: a chained `case` pattern `*" a "*" b "*`
never matches, since adjacent members share one space, and the same script
fails on one anywhere under `tests/e2e/`. A `run-all.sh` whose setup starts a
port-forward installs its EXIT trap before sourcing that setup.
`tests/e2e/common/test-port-forward.sh`, run by `task e2e:tags:check`, drives
the helpers against a stand-in kubectl, fails on any port-forward started
outside them (a line continued from the one before and `"$KUBECTL"` included),
and fails on a runner that sources such a setup before its trap.

## An e2e metrics check drives the event, then scrapes the pod that did it

prometheus-client encodes no `# HELP`, `# TYPE` or sample line for a family
with no label set, so a component that has not yet counted the event serves no
trace of the family. An e2e metrics assertion therefore drives the event first
and then scrapes `pod/<name>` of the pod that performed it: the CSI driver on
the mounting node; for the operator, the pod the leader lease names, after
touching a MachineConfig, retried up to `E2E_METRICS_TRIES` times with the
lease read again each time. OP-LC-02 reads the lease up to `E2E_LEASE_TRIES`
times (6 by default, 5 s apart), since a deleted pod keeps the lease until it
expires. The lease names a pod only because every operator
workload manifest passes `POD_NAME` and `POD_NAMESPACE` through the downward
API outside any template conditional;
`every_operator_workload_manifest_names_its_pod_through_the_downward_api`
(`crates/cfgd-operator/src/runtime.rs`) scans every YAML file in the
repository for them. A Service routes each connection to any replica, and a
`# HELP` line is absent until the first sample, so a check reads neither.
`a_family_with_no_sample_is_not_rendered` in `crates/cfgd-csi/src/metrics.rs`
holds the library to this.

## An e2e case asserts what it names, against the PR install

The operator and full-stack suites drive this run's operator and CSI driver in
the PR install: every target they read comes from `common/helpers.sh`
(`$E2E_INSTALL_NS`, `$E2E_OPERATOR_PODS`, `$E2E_OPERATOR_DEPLOY`,
`$E2E_WEBHOOK_SVC`, `$E2E_CSI_DS`, `$E2E_CSI_PODS`, `$E2E_VALIDATING_WEBHOOK`,
`$CSI_DRIVER_NAME`), OP-PR-01 fails when `$E2E_OPERATOR_DEPLOY` runs an image
other than `e2e_image cfgd-operator`, the full-stack setup stops when
`$E2E_CSI_DS` is not ready or `cosign` is missing so no full-stack case calls
`skip_test` (the test fails on one), and
`tests/e2e/common/test-pr-install.sh` fails on a tracked `*.sh` under
`tests/e2e/operator/` or `tests/e2e/full-stack/` that spells by hand `cfgd-system`, `$CFGD_NAMESPACE`,
the `app=` or `app.kubernetes.io/name=` selector for `cfgd-operator`,
`cfgd-operator` after any kubectl spelling of a Deployment, Endpoints or
Service (`deploy`, `deployments.apps`, `ep`, `svc`, ...), either release
webhook configuration, or `csi.cfgd.io`. A full-stack line that names the
release gateway `cfgd-server`, or the fleet and drift objects that live beside
it in `cfgd-system`, is listed by file and text in
`tests/e2e/common/release-targets-kept.tsv` with its reason, one row per line
it clears; a further line with the same text fails, and so does a row no line
uses. `tests/e2e/README.md` lists the spellings. A suite that applies
run-labelled objects calls `require_release_webhooks_scoped` in its setup, which
stops the suite when the release's webhooks are not scoped away from them.
Every `helm install` and `helm upgrade` in the full-stack suite passes
`"${HELM_SCOPE[@]}"`, which `helm_test_ns` composes, and no later flag sets
`operator.watchLabelSelector`, `webhook.objectSelector`,
`mutatingWebhook.namespaceSelector` or their parent; the array is exactly the
three flags that set them to the install's own `cfgd.io/e2e-helm=$HELM_NS`
label, defined once and never appended to or rewritten, so a test install's
operator, validating webhook and pod injector act only on objects and
namespaces carrying it, and an object a case wants its own install to act on
carries that label. `tests/e2e/common/test-pr-install.sh` names each install,
upgrade or array that does not.
`tests/e2e/pr-install-down.sh` removes the PR install after every cluster suite
(the `e2e-teardown` job in e2e.yml, the `defer` in `task e2e`): the run-labelled
cfgd.io objects first, while the run's operator can still clear their finalizers,
then the Helm release, then `$E2E_INSTALL_NS`, with each step run after a failed
one and the script exiting 1 at the end. Its delete lists name every plural
`schemas/crds.yaml` declares; `test-pr-install.sh` drives it against stub kubectl
and helm and fails when a CRD's plural is missing from them.
The Crossplane suite writes nothing ArgoCD tracks in `crossplane-system`: it checks
the XRD with `check_pr_xrd`, installs `$E2E_FUNCTION` from `e2e_image function-cfgd`
and `$E2E_COMPOSITION` from `render_run_composition`, every TeamConfig carries
`${E2E_RUN_LABEL_YAML}` and sets `spec.crossplane.compositionRef.name: ${E2E_COMPOSITION}`,
and the suite's end and `pr-install-down.sh` delete both and read back that they are gone;
`test-pr-install.sh` fails on a TeamConfig without the label or the reference, on a line
that applies `manifests/crossplane`, runs `kubectl` on `function-cfgd` or
`teamconfig-to-machineconfigs` or writes `teamconfigs.cfgd.io`, on a heredoc holding one
of ArgoCD's four Crossplane objects, and on a `:latest` function package.
`create_e2e_namespace` waits up to 120s for a namespace an earlier teardown of the
same run id is still deleting and stops when it outlasts that, so a re-run never
takes a Terminating namespace as created.
Every `ERROR` line an e2e script prints goes to stderr, on its own command
(`>&2`, `1>&2`, `>/dev/stderr`) or through the closer of an enclosing compound
(`} >&2`, `) >&2`, `fi >&2`, `done >&2`, `esac >&2`), whatever prefix words,
function header or case pattern come before the echo;
`tests/e2e/common/test-pr-install.sh` names each one that does not.
The gateway suite runs ArgoCD's pinned images. A case there for
behaviour only a newer build has reads the running capability and calls
`skip_test` naming `running_image <kind> <name> <container>` when it is
missing. No case calls `pass_test` after printing why the checked thing did not
happen ("Note:", "not yet", "acceptable", ...); it asserts what the note
excuses or fails. `tests/e2e/common/test-verdicts.sh` (`task e2e:tags:check`)
enforces this; a pass that is sound anyway carries `# verdict-ok: <why>` on its
`pass_test` line.

## An e2e operator object carries the run label

Every object of a kind `schemas/crds.yaml` declares that an e2e suite applies
carries `${E2E_RUN_LABEL_YAML}` in its own `metadata.labels`, and reaches the
cluster only from a heredoc with an unquoted delimiter on the apply command
itself: `kubectl apply|create|replace -f - <<EOF`, or a wrapper function a
scanned script defines whose body applies stdin. Any other apply in a suite that applies operator
objects (a manifest by path, or stdin fed by a redirect, a here-string or a
pipe) fails the check.
`tests/e2e/common/test-pr-install.sh` (`task e2e:tags:check`) reads each
heredoc body as bash sends it with mikefarah `yq` v4, which it needs on PATH,
and fails where it cannot see what is applied: an expansion in a kind or
apiVersion or standing for a whole document, a body line continued with `\`,
a script that sets `E2E_RUN_LABEL_YAML` itself. `tests/e2e/README.md` lists
every rule.

## A test never inherits its terminal shape from the ambient one

`cargo test` from a pipe and `script -qec "cargo test" /dev/null` (a real pty)
are two different terminals, and a test that reads either one asserts about how
the suite was started and says nothing about what the code did. Three ambient
inputs have to be supplied by the test, and a test inherits none of them:

| Ambient input | Supply it with |
|---|---|
| **Colour** — a styled render used to re-read `console::colors_enabled()`, which is on under a pty | a `Printer::for_test*` constructor (all pin `colors: false`); `for_test_with_theme_colored` is the one that pins it ON |
| **Live region** — a spinner's start line is written when there is none and repainted away when there is | a `Printer::for_test*` constructor (they pin `live_region: false`); the three `live_capture` constructors — `for_test_live_scrollback`, `for_test_with_live_bars`, `for_test_live_terminal` — pin it ON, and which one to reach for is in `shared-utils.md`'s Test guards section |
| **stdin TTY** — the interactive-script gate | `execute_script_with_tty(stdin_is_tty, …)`; the `execute_script` wrapper reads `stdin().is_terminal()` and is not used |

Colour is decided ONCE, per `Printer`, at construction, and folded into its theme
(`Theme::with_colors`), so by construction a capture buffer cannot be styled, and no
convention is needed to strip it. Production supplies the decision as a
`ColorChoice` (`Auto` resolves `console`'s detection minus
`output::printer::colors_must_be_disabled(&format)`; `--no-color` passes `Never`).
That veto covers only the formats whose payload is a machine contract
(`OutputFormat::refuses_color`): `-o yaml` is highlighted under the ordinary
decision and asks STDOUT (stderr plays no part), because the payload is the only
thing colour reaches there. Every capture constructor supplies `false` except
`for_test_with_theme_colored` and `for_test_with_theme_and_format(.., colors)`. No PRODUCTION code writes `console`'s colour
flags, so nothing a run does can change what a printer already decided. Tests write them
through exactly one guard — `output::printer::ColorGlobalOn`, which restores the prior
values on drop including on unwind — and only to reproduce the flags being ON as the
reported condition: `a_flipped_colour_global_cannot_style_a_capture` proves a capture
stays unstyled anyway, `a_colourless_printer_draws_a_colourless_progress_bar` proves
indicatif's own template resolution leaks NO escape past `--color never` — a style
token is one whatever it names, so the colourless template carries none and the
filled/empty contrast comes from `progress_chars` instead — and
`derived_printers_inherit_the_colour_decision` proves a derived printer does not re-read
them. Never hand-roll a second save/restore struct; pair the guard with
`serial_test::serial`.

The same pairing is the rule for the environment itself: a test that mutates a
process-global env var — through `EnvVarGuard`, `EditorGuard::set`,
`with_test_env_var`, `ProbePath::containing`, any `install_named_path_shim*`,
either tool shim, or a raw `env::set_var` / `remove_var`, in its own body or in a
same-file helper it calls — carries `#[serial_test::serial…]`, because edition 2024 makes
an unserialized write in a live-threaded harness undefined behaviour, which is
worse than a flake; `every_test_mutating_the_process_environment_serializes_itself` walks
for one, and `// serial-ok: <why>` hatches a mutation that cannot race (a
per-child `Command::env(…)` handoff is not one of them and is never matched).

Two serial attributes on one declaration are two locks, taken in the order they
are written, so every declaration writes that order the same way: the unnamed
lock first, named groups alphabetically. The other order holds one lock while it
waits for the other against a sibling doing the reverse, and both tests hang with
no timeout to fire;
`every_declaration_taking_two_serial_locks_takes_them_in_one_order`
(`output/tests/fences.rs`) walks every crate for an inverted pair and has no
hatch.

A source walk reads each production file through `production_slice_of` or
`floored_production_body`; when it reads a whole file, inline
`#[cfg(test)]` included, it calls `walked_file_body` and carries
`// unfloored-slice-ok: <why>`. Both guarded readers slice a test-only file to nothing.
`every_multi_file_production_walk_reads_through_the_floored_helper` fails a raw
`read_to_string` in any source walk, and a `walked_file_body` read there without the hatch.
Its `FLOORS` table holds a row per crate at today's count of source walks, sources
spelling the pure cut, and reads through `floored_production_body` and
`production_and_seams_of`; a crate holding a walk without a row fails it.

Each walk picks its view by its question. A walk asking what SHIPS reads the production
view (`production_slice_of`, `workspace_declarations`). A walk asking what a TEST can
drive (a call-graph fold that decides which tests must guard a verb, a seam-read guard)
reads the seam view (`production_and_seams_of`, `workspace_seam_declarations`), which
keeps every `test-helpers` item: a seam like `run_compliance_and_reconcile_ticks` is
production code compiled for tests, and a production-only fold never sees the route a
test takes through it. A walk judging code folds nothing itself: it reads
`production_code_of(path)` or a memo row's `code_of(i)`, both cut from the file's one scan.
A walk asking about TEST text reads `test_region_of(path)`, or `line_gates_of(path)` when it
partitions a file by index; one attribute is judged by `attribute_gate`. A
string search for a gate's spelling (`.starts_with("#[cfg(test)]")`, `.contains("mod tests")`,
or a needle any binding carries to one: `let`, `let … else`, `const`, `static`, `for`, `if let`,
`while let`, `match` arm, assignment, closure or function parameter, macro argument, format
capture; or a function's return, a `self.f` field or `self.m()` return, a `const` another source
declares, a `concat!` of literals or a byte string, or a `format!` whose template holds only bare
`{}` placeholders over literal arguments) is a second cut beside the scanner, and the
floored-helper walk fails it in test scope. The walks' own searches reaching a gate's spelling, in
fences.rs and in test_helpers.rs's test region, are declared by file and function, with their
count, in `OWN_GATE_SEARCHES`.

Colour off means NO escapes — attributes included. `ThemedStyle::apply_to` is the ONE
gate a styled span becomes bytes through, and a printer whose `ColorChoice` resolved
`false` gets bare text: bold, dim, italic, underline and OSC 8 are withheld with the
foreground, because an attribute is styling too and `docs/cli-reference.md` promises
`--color never` / `NO_COLOR` / a non-terminal stdout withhold every escape. An indicatif
template is a second escape writer and answers to the same decision. Pinned by
`no_escape_reaches_a_stream_the_printer_decided_against`, the bar pin above, and the walk
beside them, `every_styled_span_reaches_bytes_through_the_one_gate` — whose `WRITES` list
is the CLASS (every literal notation, the raw byte, `ProgressStyle::with_template`,
`console::Style`), whose `GATED` needle is `apply_to` itself, and whose escape hatch is
`// style-gate-ok: <why>` read off the whole comment block above the line.

Strip anyway when the assertion is about TEXT: `captured_text` is still the ONE read of
a capture buffer, because `for_test_with_theme_colored` deliberately forces styling ON
and really does emit escapes. Read the buffer raw only when the assertion is ABOUT the
escapes; to assert the colour DECISION call `colors_must_be_disabled(&format)` and
render nothing.

Goldens are captured through a path where all three are pinned — `assert_human_snapshot*`
strips for its caller, while the raw `assert_snapshot_at` does not, so a caller reaching
it directly strips first. Goldens are RE-CAPTURED (`INSTA_UPDATE=always`); nobody
edits one by hand.

Verify both ways before calling a test suite green; a suite only ever observed one way is
how all of this shipped.

## A scoped tracing capture installs the process-global journal under it

`tracing` caches one `Interest` per callsite for the whole process and computes it
from what the thread that first reaches the callsite can see. While a single
dispatcher is registered, a callsite first reached from a thread holding no
subscriber at all caches `never`, and every later event there is dropped until an
unrelated registration rebuilds the cache — including the event a capture on
another thread is waiting for, which reads back empty. So a test binding a scoped
subscriber (`tracing::subscriber::with_default`, `WithSubscriber`) calls
`cfgd_core::test_helpers::install_tracing_journal()` first, and
`every_scoped_tracing_capture_installs_the_journal_under_it`
(`output/tests/fences.rs`) walks every crate for a bind with no installer above
it.

The journal that installer leaves behind is also what the daemon's `run_daemon`
loop tests read: those lines come from tokio tasks and watcher threads the daemon
owns, which a scoped dispatcher never reaches. A reader of it asks only whether a
line APPEARED — it carries no target filter, so every event any test in the binary
emits reaches it, and an absence or a count answers by whatever else the run
scheduled. A line read as proof that the reader's OWN subject reached a state
needs more than containment: every declaration that starts a daemon joins the
`tracing_dispatcher` group, because a sibling's daemon writes the same startup
banner.

That group belongs to the journal alone. A scoped capture holds one thread's
buffer for the length of one closure, so a test whose only tracing reach is a
capture carries no serial attribute: what keeps its verdict independent of what
another thread registered is the journal installed under it, and no lock is involved.

## A fail-without-fix probe never mutates the shared working tree

Proving a test fails without its fix means breaking the production code and watching
the test go red. Doing that **in place** — edit, `cargo test`, restore — leaves the
repository holding deliberately broken code for the length of a compile, and anything
else reading the tree in that window (a second agent, a watch build, a full-workspace
run someone else started) compiles the broken revision and reports failures that
describe nothing anybody wrote.

That is not hypothetical: two daemon advisory-restatement tests were reported failing
under a full `--test-threads=16` workspace run, with counts (`0 of 3` and `1 of 3`)
that exactly reproduced an in-tree probe of `CachedConfig::advisories_to_restate`
returning `&[]`. Six later runs of the same binary at the same thread count were green,
and the failure was never a concurrency defect at all — it was a probe window.

Copy the tree first — **excluding `target/`**, which is tens of GB and duplicating it
filled the shared VM's root filesystem to 100% mid-CI — and give the copy its own
target dir:

```bash
rsync -a --exclude=/target /opt/repos/cfgd/ ~/.cache/cfgd-debug/probe/
CARGO_TARGET_DIR=~/.cache/cfgd-debug/probe-target \
  cargo test --manifest-path ~/.cache/cfgd-debug/probe/Cargo.toml -p cfgd-core --features test-helpers --lib <filter>
```

The evidence is identical and no other reader can see the mutation. Scratch goes under
`~/.cache/` (`/tmp` is off limits), and the probe TREE is deleted as soon as the probe's red run is
captured, so nothing later reads a tree still carrying a deliberate defect. The shared
target dir (`~/.cache/cfgd-debug/red-target`) is retained: a fresh tree is copied per
probe, so a kept target dir changes only rebuild cost and leaves what a probe measures alone.

## An expectation built from host-dependent bytes goes through the producer's own fold

A test runs on Linux, macOS and Windows, so an expected line is built the way the
product builds it:

| Expected bytes | Build them with | Pin (`output/tests/fences.rs`) |
|---|---|---|
| a path the renderer home-folds | `fold_home_in_text(&to_posix_string(p))` | `no_home_fold_is_handed_a_native_path_render` |
| a line of an env file this host generates | `MergedEnvItems::declared_line(kind, name)` | `no_fixture_hand_spells_a_line_of_the_env_file_this_host_generates` |
| the env reminder an apply prints (``Run `source …` ``) | `EnvVarGuard` on both `MSYSTEM` and `SHELL` (`MSYSTEM` set to a non-blank literal decides alone), then the platform's command | `no_test_reads_the_env_reminder_under_the_ambient_shell` |
| a file a clone checked out, read in either operand of an equality assert or through a binding | `normalize_line_endings(&read)` (the user's git config decides EOL) | `no_cloned_file_is_compared_byte_for_byte` |

A byte-exact compare of a file the test wrote itself in a cloning test carries
`// eol-exact-ok: <why>` on the line above the assert.

## Fixture versions: use the 9.9.x sentinel range

When a test hardcodes a version string as a scaffold (mock upgrade
flows, fake release tags, illustrative bump scenarios) and does not
assert against `CARGO_PKG_VERSION`, use a version in the **9.9.x**
range (e.g. `v9.9.0`, `v9.9.1`). These never coincide with any real
cfgd release stream, so the test stays inert across version bumps.

A real bump (`0.3.5 → 0.4.0`) once silently broke `upgrade_bridge_one_blank_line`
because the test body hardcoded `"Upgraded to v0.4.0"` as a fixture
that happened to match the project's actual target. Reverting the
project version flipped the test red even though nothing about the
*formatting invariant* the test claimed to check had changed.

Tests that DO assert against real `CARGO_PKG_VERSION` (e.g.
`upgrade_check_up_to_date_human` exercising `cmd_upgrade`) keep their
snapshots tracking the real version — those are correctly coupled.
The sentinel rule applies only to test-body literal fixtures.

## A pin that runs at one uid says so in its name

A test whose first statement returns early on `cfgd_core::is_root()` executes nothing
at the other uid, and its green line is indistinguishable from a pin that ran. The name
carries which half executed, with a mechanical suffix read off the gate:

| First statement | Suffix |
|---|---|
| `if !is_root() { return; }` | `_as_root` |
| `if is_root() { return; }` | `_as_non_root` |
| `if !(cfg!(target_os = "linux") && is_root()) { return; }` | `_as_linux_root` |
| `if cfg!(target_os = "linux") && is_root() { return; }` | `_as_non_linux_root` |

A pin that asserts in both arms (`if is_root() { assert A } else { assert B }`) takes no
suffix: every run executes it. The suffix also binds the other way, so a name claiming a
uid must open on that gate. `every_pin_that_runs_at_one_uid_says_so_in_its_name`
(`output/tests/fences.rs`) walks every crate in both directions and floors each suffix
at the count the workspace holds.
