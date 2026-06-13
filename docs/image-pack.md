# `cfgd image pack`

Pack a directory into a standard OCI image and push it to a registry. The result is
mountable as a Kubernetes `volume.image` ([KEP-4639](https://github.com/kubernetes/enhancements/issues/4639))
via containerd's native image volume support — no Dockerfile, no Docker daemon.

**Design boundary:** cfgd packs *already-produced* directories. It does not run compilers
or package managers. CI produces the directory (`go build` → binary, `pip install --target`
→ site-packages, `npm ci` → node_modules); `cfgd image pack` turns that directory into a
digest-pinned OCI image. Capability and config layers become independently swappable image
volumes on top of a hardened shared base, instead of a per-app Dockerfile.

## Usage

```sh
cfgd image pack <DIR> <ARTIFACT> [flags]
```

## Quickstart

```sh
# Build the binary in CI, pack it into a registry image, sign it.
go build -o ./out/server ./cmd/server

$ cfgd image pack ./out registry.example.com/myapp/server:v1.4.0 \
    --entrypoint /app/server \
    --env PORT=8080 \
    --sign

Pack Image
  Directory  ./out
  Artifact   registry.example.com/myapp/server:v1.4.0
  Digest     sha256:3a7b9c4d...
✔ Signed artifact with cosign
✔ Packed and pushed registry.example.com/myapp/server:v1.4.0
```

### Structured output (`-o json`)

```sh
$ cfgd image pack ./out registry.example.com/myapp/server:v1.4.0 \
    --sign --attest \
    -o json
```

```json
{
  "artifact": "registry.example.com/myapp/server:v1.4.0",
  "digest": "sha256:3a7b9c4d...",
  "platform": "linux/amd64",
  "signed": true,
  "attested": true
}
```

## Flags

| Flag | Description |
|---|---|
| `--platform <os/arch>` | Target platform (default: host platform, e.g. `linux/amd64`) |
| `--entrypoint <arg>` | Image entrypoint (repeatable; builds a list, e.g. `--entrypoint /app/server`) |
| `--cmd <arg>` | Default command arguments (repeatable) |
| `--env KEY=VALUE` | Environment variable in the image runtime config (repeatable) |
| `--working-dir <path>` | Working directory for the entrypoint |
| `--user <user>` | User/UID for the entrypoint (e.g. `nobody`, `1000`) |
| `--label k=v` | Image config label (repeatable; `→ config.Labels`) |
| `--annotation k=v` | Manifest annotation (repeatable; `→ manifest.annotations`) |
| `--sign` | Sign the pushed image with cosign (keyless by default) |
| `--key <path>` | Signing key path (used with `--sign` or `--attest`) |
| `--attest` | Attach a SLSA provenance attestation |
| `--lock [<file>]` | Record the resolved digest in an image lockfile (default `cfgd-images.lock`) for `kubectl cfgd deploy` |

Global `-o` / `--output` applies: `json`, `yaml`, `jsonpath`, and `go-template` are all
supported. See [Global flags](configuration.md#global-flags).

## OCI correctness

The produced image uses standard OCI media types:

| OCI field | Value |
|---|---|
| Config `mediaType` | `application/vnd.oci.image.config.v1+json` |
| Layer `mediaType` | `application/vnd.oci.image.layer.v1.tar+gzip` |
| Manifest `mediaType` | `application/vnd.oci.image.manifest.v1+json` |

The image config carries `architecture`, `os`, and `rootfs.diff_ids`. The `diff_ids` are
SHA256 digests of the **uncompressed** tar — the value containerd validates on unpack. The
layer descriptor digest is over the gzipped bytes. Both hashes are computed from a single
pass through the archive; the standard OCI double-hash is handled internally. The result
is identical in structure to any crane-built or buildkit-built image, and containerd's GC,
pull cache, and credential helpers all apply normally.

## Worked example: `gome` (Go binary service)

This example follows the pattern described in the [composable-services design doc](.claude/plans/2026-06-04-cfgd-composable-services-vs-dockerfiles.md):
CI compiles, `cfgd image pack` packages, a hardened Pod mounts the result.

### CI workflow (`.github/workflows/pack.yml` excerpt)

```yaml
- name: Build binary
  run: CGO_ENABLED=0 go build -o ./out/gome ./cmd/gome

- name: Build assets
  run: npm ci --prefix web && cp -r web/dist ./out-assets/

- name: Pack binary image
  run: |
    cfgd image pack ./out \
      registry.jarvispro.io/gome/server:${{ github.sha }} \
      --entrypoint /app/gome \
      --sign --attest \
      -o json | tee pack-server.json

- name: Pack assets image
  run: |
    cfgd image pack ./out-assets \
      registry.jarvispro.io/gome/assets:${{ github.sha }} \
      --sign --attest \
      -o json | tee pack-assets.json
```

Each push produces a distinct digest. Pin those digests in your deployment; the binary
and asset layers are independently swappable — editing a CSS file repackages only the
assets layer; the binary image stays byte-identical in every node's cache.

### Pod spec — hardened, distroless, no Dockerfile

```yaml
apiVersion: v1
kind: Pod
spec:
  # root-in-pod ≠ root-on-node (KEP-127, beta-on-default in k3s 1.33)
  hostUsers: false
  volumes:
    - name: app
      image:
        reference: registry.jarvispro.io/gome/server@sha256:3a7b9c4d...
        pullPolicy: IfNotPresent
    - name: assets
      image:
        reference: registry.jarvispro.io/gome/assets@sha256:7f2e1a9b...
        pullPolicy: IfNotPresent
    - name: tmp
      emptyDir: {}         # writable /tmp over a read-only root
  containers:
    - name: gome
      image: gcr.io/distroless/static:nonroot   # ca-certs + nonroot user, fleet-shared base
      command: ["/app/gome"]
      env:
        - name: PORT
          value: "8080"
      securityContext:
        readOnlyRootFilesystem: true
        capabilities:
          drop: [ALL]
        seccompProfile:
          type: RuntimeDefault
      volumeMounts:
        - name: app
          mountPath: /app
          readOnly: true
        - name: assets
          mountPath: /app/web
          readOnly: true
        - name: tmp
          mountPath: /tmp
```

**What disappears compared to a Dockerfile:**

| Dockerfile layer | Equivalent here |
|---|---|
| `FROM alpine:3.19` | `image: gcr.io/distroless/static:nonroot` (fleet-shared) |
| `RUN apk add ca-certificates curl` | included in the distroless base |
| `RUN adduser -D -u 1000 gome` | `nonroot` tag bakes this in |
| `COPY /gome /app/gome` | `volume.image` mount of `gome/server@sha256:…` |
| `COPY /web /app/web` | `volume.image` mount of `gome/assets@sha256:…` |
| `ENTRYPOINT ["/app/gome"]` | `command:` field |

**Cluster prerequisite:** the `ImageVolume` feature gate must be enabled on all nodes.
On k3s, add `feature-gates=ImageVolume=true` to `/etc/rancher/k3s/config.yaml` under
both `kube-apiserver-arg` (master) and `kubelet-arg` (all nodes), then restart k3s/k3s-agent.
Verify with `kubectl get --raw /metrics | grep 'kubernetes_feature_enabled{name="ImageVolume"}'`.
In Kubernetes 1.33 the gate is beta but off by default; it is expected to be on-by-default
in a future release.

## Closed-loop pinning: `--lock` + `kubectl cfgd deploy`

Pinning the exact bytes you packed is a two-step loop. `cfgd image pack --lock` records the
resolved digest in an image lockfile; `kubectl cfgd deploy` rewrites the mutable
`volumes[].image.reference` tag in your manifests to that pinned digest — so you deploy the
artifact you tested, not whatever the tag happens to point at later.

### Step 1 — pack with `--lock`

```bash
$ cfgd image pack ./out \
    registry.jarvispro.io/gome/server:abc123 \
    --entrypoint /app/gome --lock
Pack Image
  Directory  ./out
  Artifact   registry.jarvispro.io/gome/server:abc123
  Digest     sha256:3a7b9c4d...
  Locked     cfgd-images.lock
✓ Packed and pushed registry.jarvispro.io/gome/server:abc123
```

`--lock` writes (or upserts, matched by `reference`) an entry into `cfgd-images.lock` in the
current directory. Pass a path (`--lock path/to/file`) to override the location.

```yaml
# cfgd-images.lock
images:
  - reference: registry.jarvispro.io/gome/server:abc123
    digest: sha256:3a7b9c4d...
    pinned: registry.jarvispro.io/gome/server@sha256:3a7b9c4d...
    lockedAt: 2026-06-13T12:00:00Z
```

### Step 2 — deploy with the pinned digest

`kubectl cfgd deploy` reads the lockfile and rewrites every `volumes[].image.reference`
whose tag matches a locked entry — at any depth, so bare-Pod `spec.volumes[]` and workload
`spec.template.spec.volumes[]` shapes both get pinned. By default it prints the rewritten
manifest to stdout (pipe it to `kubectl`); `--apply` runs `kubectl apply` directly.

```bash
# Print mode (default) — stdout is a clean pipe
$ kubectl cfgd deploy -f pod.yaml --lock cfgd-images.lock | kubectl apply -f -

# Apply directly into a namespace
$ kubectl cfgd deploy -f pod.yaml --apply -n prod
✓ Applied 1 document(s), 1 reference(s) pinned
```

The rewrite, before and after:

```yaml
# pod.yaml (as authored — mutable tag)
volumes:
  - name: app
    image:
      reference: registry.jarvispro.io/gome/server:abc123
```

```yaml
# emitted by `kubectl cfgd deploy` (pinned to packed bytes)
volumes:
  - name: app
    image:
      reference: registry.jarvispro.io/gome/server@sha256:3a7b9c4d...
```

In `-o json` mode the rewritten manifest is returned as a `manifest` field alongside a
`rewrites` array, so CI can consume the pin set programmatically. References not present in
the lockfile are left untouched.

## Signing and attestation

`--sign` calls cosign after the push. By default, cosign uses keyless signing
(Fulcio/OIDC + Rekor) — suitable for CI with a GitHub Actions OIDC token. Pass
`--key <path>` to use a local signing key instead.

`--attest` generates a SLSA provenance statement (repo URL + git HEAD, detected from
the working tree's `origin` remote and `HEAD` ref) and attaches it as a cosign
attestation. Requires cosign on `PATH`.

Both flags are independent and can be combined: `--sign --attest` signs the image
manifest and attaches the provenance in one invocation.

## Error output

| Error kind | Meaning |
|---|---|
| `not_found` | `<DIR>` does not exist |
| `invalid` | `<DIR>` exists but is not a directory |
| `invalid_label` | `--label` value is not `KEY=VALUE` |
| `invalid_annotation` | `--annotation` value is not `KEY=VALUE` |
| `pack_failed` | tar, registry upload, or manifest push failed |
| `sign_failed` | cosign sign step failed |
| `attest_failed` | cosign attest step failed |

In structured mode (`-o json`) every error carries `"artifact"` and the error kind so
scripted consumers can route failures without parsing stderr. See [Error output](cli-reference.md#error-output).
