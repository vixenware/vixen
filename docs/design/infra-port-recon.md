# Infra port recon: vixen-infra

Subject path requested: `~/vixen/vixen-infra`.

That exact path does not exist on this machine. The real checkout is:
`/Users/amos/vixenware/vixen-infra`.

Required context read first:

- `docs/design/infra-as-demand.md`: journal-as-state, addressed effects with
  custody, reconcile over probed reality, standing demand, field custody,
  standing runs, leases, and Rodin-as-scheduler.
- `docs/design/artifact-intelligence.md`: artifact probes, OCI-as-merged-tree,
  probe-at-acquire, and journal/probe corroboration.

This is a recon and design-sketch pass only. The committed `.vix` files under
`docs/design/sketches/infra/` are communication sketches; they are not parser
fixtures.

## Repo Map

`vixen-infra` is not a Terraform repo. There are no checked-in `*.tf`,
`*.tfvars`, or `*.hcl` files in the checkout. Terraform/OpenTofu appears as an
external dependency for DNS in the `cove/infra` repo, not as machinery owned by
this repo (`STATE.md:264-272`, `RUNBOOK.md:27-34`,
`notes/04-dns-tls.md:7-18`).

The repo has four real machinery layers:

| Layer | What is in it | Primary evidence |
|---|---|---|
| Human state and runbooks | Point-in-time state, phased notes, rebuild runbook, integration notes | `README.md:3-7`, `STATE.md:1-8`, `RUNBOOK.md:1-8`, `notes/README.md` |
| Host automation | Ansible inventory, site playbook, Debian roles, macOS role, SOPS vars | `ansible/README.md:29-43`, `ansible/inventory.yaml:1-63`, `ansible/site.yaml:16-66`, `.sops.yaml:10-19` |
| Kubernetes GitOps | k3s bootstrap manifests, Argo app-of-apps, Argo Applications, Kustomize dirs | `clusters/prod-internal/bootstrap/root.yaml:1-48`, `clusters/prod-internal/bootstrap/argocd.yaml:1-73`, `clusters/prod-internal/apps/app-vixen.yaml:1-22` |
| Host reference scripts/units | systemd/launchd/WinSW units, restic scripts, Docker/nftables/sysctl references | `STATE.md:274-294`, `host/restic/restic-backup.sh:1-44`, `host/windows-forgejo-runner/README.md`, `host/macos-forgejo-runner/README.md` |

## Managed Reality

Hosts:

- `jamie`: Hetzner dedicated server, control plane and public services
  (`README.md:9-22`, `STATE.md:35-48`).
- `hazel` and `ash`: dedicated CI nodes joined to k3s with CI taints
  (`ansible/inventory.yaml:31-48`, `STATE.md:49-81`).
- `ahri`: macOS runner in the Ansible `macos` group
  (`ansible/inventory.yaml:49-63`, `ansible/site.yaml:31-66`).
- `toast`: Windows home runner documented as host-mode WinSW service
  (`STATE.md:83-101`).

Host configuration:

- Admin user, SSH hardening, nftables, sysctl, fail2ban, unattended upgrades
  (`ansible/roles/base-os/tasks/main.yaml:11-124`).
- Docker on a ZFS data root, public DNS override, live bridge-DNS probe, QEMU
  binfmt for cross-arch CI (`ansible/roles/docker/tasks/main.yaml:1-134`).
- Host-level Forgejo runners: user, cache datasets, pinned/checksummed binary,
  one-shot registration, systemd service, registry auth, prune timer
  (`ansible/roles/forgejo-runner/tasks/main.yaml:1-207`).
- Grafana Alloy host push agent for metrics/logs
  (`ansible/roles/monitoring-agent/tasks/main.yaml:1-76`,
  `ansible/README.md:246-271`).

Cluster:

- k3s is the cluster substrate, administered over Tailscale; Argo CD is
  bootstrapped by k3s HelmChart and then owns the cluster through an app-of-apps
  root (`RUNBOOK.md:36-86`, `clusters/prod-internal/bootstrap/root.yaml:1-48`).
- The root Application recurses through `clusters/prod-internal/apps/` and
  enables automated sync, prune, and self-heal
  (`clusters/prod-internal/bootstrap/root.yaml:27-48`).
- Tailscale operator is also bootstrap-tier because it provides the admin access
  path (`clusters/prod-internal/bootstrap/tailscale-operator.yaml:1-30`).

Services and operators:

- Argo CD, cert-manager, Tailscale operator, CloudNativePG, Barman plugin,
  kube-prometheus-stack, Loki, Alloy, Gatus, Traefik/CoreDNS config, oauth2-proxy,
  Forgejo, Zulip, app-vixen/admin-vixen, kb-vixen, vixen-rs, bee-site,
  ccc-vixen, Bugsink, Listmonk, Valkey, Docker Hub cache, Forgejo/Linear bridge,
  ARC runner pools, and Hetzner Cloud autoscaler are all represented as Argo
  Applications or Kustomize manifests under `clusters/prod-internal/apps/` and
  `clusters/prod-internal/manifests/`.

Secrets:

- One root SOPS/age recipient covers both Ansible and Kubernetes paths
  (`.sops.yaml:1-19`, `ansible/README.md:166-190`).
- Argo CD decrypts `*.sops.yaml` via KSOPS with an age private key mounted into
  the repo-server (`clusters/prod-internal/bootstrap/argocd.yaml:37-73`).
- Bootstrap secrets cannot come from Git: the SOPS private key, Tailscale OAuth
  client, and Argo deploy key must exist before Argo can sync
  (`RUNBOOK.md:42-73`).
- Runtime secrets include Scaleway backup credentials, GitHub App keys, Forgejo
  registry pull secrets, OAuth/OIDC secrets, Stripe keys, SMTP credentials, and
  restic credentials (`RUNBOOK.md:9-25`, `RUNBOOK.md:189-225`,
  `RUNBOOK.md:227-260`, `notes/05-backups.md:28-64`).

Networking, certs, and DNS:

- Public ports are intentionally tiny: 80/443 for web and 22 for Forgejo git-SSH;
  host SSH moves to 2222 and stays tailnet-only (`STATE.md:135-149`,
  `notes/01-base-os-hardening.md:139-148`).
- k3s pod networking uses flannel and Tailscale for cross-node traffic; CI nodes
  must advertise tailnet IPs and `tailscale0` as flannel iface
  (`STATE.md:150-170`, `notes/18-github-oss-runners.md:58-120`).
- Public DNS is managed outside this repo via Cloudflare/OpenTofu in `cove/infra`
  (`notes/04-dns-tls.md:7-18`, `RUNBOOK.md:27-34`).
- cert-manager issues public `vixen.rs` certificates via HTTP-01 through Traefik;
  Tailscale `.ts.net` names use Tailscale certs instead
  (`clusters/prod-internal/manifests/cert-manager/clusterissuers.yaml:1-35`).
- CoreDNS is patched by Server-Side Apply for `data.Corefile` only, while k3s
  keeps ownership of `data.NodeHosts`
  (`clusters/prod-internal/manifests/coredns/coredns-cm.yaml:1-40`).

Storage and data:

- `jamie` uses mdraid1 for the OS and ZFS `tank` datasets for k3s, Postgres,
  Forgejo, CI cache, and backup staging (`STATE.md:109-133`,
  `notes/01-base-os-hardening.md:150-223`).
- CNPG clusters use local-path storage plus Barman object-store backups to
  Scaleway (`clusters/prod-internal/manifests/forgejo-db/cluster.yaml:1-21`,
  `clusters/prod-internal/manifests/forgejo-db/objectstore.yaml:1-21`).
- Daily CNPG `ScheduledBackup` resources exist per database cluster
  (`clusters/prod-internal/manifests/forgejo-db/scheduled-backup.yaml:1-12`).
- Host restic backs up `/etc`, `/root`, Forgejo data PV, and Zulip uploads PV;
  CNPG PVCs are deliberately excluded because CNPG handles WAL archiving
  (`host/restic/restic-backup.sh:1-44`, `notes/05-backups.md:45-64`).

## Dependency Structure

The actual graph is:

1. Off-box prerequisites: repo, password-manager secrets, Tailscale account,
   Hetzner/Scaleway/Cloudflare/Brevo/GitHub accounts (`RUNBOOK.md:9-25`).
2. Bare-metal host provisioning: OS install, ZFS pool, admin SSH, Tailscale,
   nftables, base packages (`RUNBOOK.md:27-40`,
   `notes/01-base-os-hardening.md:7-227`).
3. k3s install: API over Tailscale, pod network, local-path PVs
   (`notes/02-k3s.md:1-40`).
4. Bootstrap secrets: SOPS age private key, Tailscale OAuth, Argo repo deploy key
   (`RUNBOOK.md:42-73`).
5. Bootstrap manifests: Argo CD HelmChart, Tailscale operator, root app
   (`RUNBOOK.md:75-86`, `clusters/prod-internal/bootstrap/argocd.yaml:1-73`,
   `clusters/prod-internal/bootstrap/root.yaml:1-48`).
6. Cluster primitives: cert-manager, ClusterIssuers, CNPG, Barman plugin,
   KSOPS, object store credentials, monitoring primitives (`STATE.md:185-203`).
7. Services: DBs and secret generators first, then workloads and ingresses.
   Some ordering is explicit via Argo sync waves, for example CNPG operator at
   wave 1, Barman object store at wave 3, Forgejo at wave 2
   (`clusters/prod-internal/apps/cnpg-operator.yaml:1-26`,
   `clusters/prod-internal/apps/cnpg-objectstore.yaml:1-22`,
   `clusters/prod-internal/apps/forgejo.yaml:1-149`).
8. Push/observe paths: Argo webhook, Argo self-heal, controller watches, Alloy
   push logs/metrics, Gatus/Prometheus alerts, Kubernetes liveness/readiness.
9. Restore/adoption paths: CNPG recovery from object store and restic restore
   into newly-created PV directories (`RUNBOOK.md:88-121`).

In Vixen terms, `STATE.md` is the hand-maintained precursor to journal state,
Argo Applications and Ansible tasks are addressed effects, and the live checks
sprinkled through the repo are probes. The port should replace "documented
operator discipline" with claims carrying observation evidence and custody.

## Representative Port Sketches

Committed sketches:

- `docs/design/sketches/infra/host-provision.vix`: `jamie` base OS, ZFS, SSH,
  firewall, Docker, runner, Alloy.
- `docs/design/sketches/infra/app-vixen-service.vix`: app image, CNPG DB,
  KSOPS secrets, Deployment, Ingress, probes.
- `docs/design/sketches/infra/dns-tls-coredns.vix`: external DNS dependency,
  cert-manager ClusterIssuer/Certificate, Traefik route, CoreDNS field custody.
- `docs/design/sketches/infra/runner-autoscaler.vix`: ARC scale sets, Rodin
  placement facts, hcloud nodes, Kubernetes leases, standing runner pods.

## Capability Ledger

| Capability the repo actually uses | Real machinery | Vixen port status |
|---|---|---|
| Point-in-time state ledger | `STATE.md` records as-built reality (`STATE.md:1-8`) | covered today: journal-as-state |
| Rebuild runbook | `RUNBOOK.md` bare-box to cluster path (`RUNBOOK.md:1-8`) | covered today: demanded root plus journal history |
| Host inventory | Ansible inventory over Tailscale MagicDNS (`ansible/inventory.yaml:1-63`) | covered today: probed hosts as artifacts |
| Bare-metal OS install | Hetzner installimage notes (`notes/01-base-os-hardening.md:7-15`) | covered today: provider capability plus addressed host effects |
| SSH/admin hardening | base-os role and notes (`ansible/roles/base-os/tasks/main.yaml:11-50`) | covered today: reconcile addressed files/services |
| Firewall policy | nftables templates and notes (`notes/01-base-os-hardening.md:82-148`) | covered today, plus field custody for generated chains |
| Kernel/sysctl tuning | base-os role (`ansible/roles/base-os/tasks/main.yaml:69-99`) | covered today: addressed file effects plus probes |
| Package install/update policy | base-os/docker/monitoring roles (`ansible/roles/base-os/tasks/main.yaml:34-40`) | covered today: capability fingerprint and reconcile |
| Docker daemon on ZFS | docker role (`ansible/roles/docker/tasks/main.yaml:1-57`) | covered today: addressed effects, probed datasets |
| Docker DNS drift probe | live bridge container probe (`ansible/roles/docker/tasks/main.yaml:61-115`) | covered today: reconcile with probed reality |
| Cross-arch binfmt | QEMU/binfmt registration (`ansible/roles/docker/tasks/main.yaml:117-134`) | covered today: host artifact probe and reconcile |
| Host Forgejo runners | runner role (`ansible/roles/forgejo-runner/tasks/main.yaml:1-207`) | STANDING RUNS: success is staying registered/alive |
| Runner one-shot registration | token consumed into `.runner` (`ansible/roles/forgejo-runner/tasks/main.yaml:87-130`) | NEW: secret/reveal lifecycle; covered today for addressed effect shape |
| Host disk hygiene | prune timers and ZFS quotas (`ansible/README.md:233-245`) | TEMPORAL policy plus standing runs |
| Host metrics/log push | Alloy role (`ansible/roles/monitoring-agent/tasks/main.yaml:1-76`) | PUSH INVALIDATION inlet plus standing runs |
| k3s bootstrap | notes/runbook (`notes/02-k3s.md:1-40`, `RUNBOOK.md:36-40`) | covered today: addressed cluster effect |
| k3s static manifests | Argo/Tailscale bootstrap (`clusters/prod-internal/bootstrap/*.yaml`) | covered today: addressed effects with custody |
| Argo app-of-apps | root Application (`clusters/prod-internal/bootstrap/root.yaml:27-48`) | STANDING-DEMAND scheduler |
| Argo prune/self-heal | Application syncPolicy (`clusters/prod-internal/bootstrap/root.yaml:43-48`) | STANDING-DEMAND scheduler |
| Argo sync waves | annotations in apps (`clusters/prod-internal/apps/cnpg-operator.yaml:7-10`) | TEMPORAL/ordering policy |
| Argo finalizers | ARC child apps (`notes/18-github-oss-runners.md:54-56`) | addressed deletion custody |
| Server-Side Apply | CNPG and CoreDNS apps (`clusters/prod-internal/apps/cnpg-operator.yaml:20-26`) | FIELD-LEVEL CUSTODY |
| Helm chart installs | Argo Application sources (`clusters/prod-internal/apps/forgejo.yaml:14-21`) | covered today: provider capability plus chart artifact probe |
| Kustomize directories | app sources (`clusters/prod-internal/apps/app-vixen.yaml:10-22`) | covered today: pure tree transforms |
| KSOPS decrypt on sync | Argo repo-server config (`clusters/prod-internal/bootstrap/argocd.yaml:37-73`) | NEW: sealed secret distribution and decrypt custody |
| SOPS/age recipient policy | `.sops.yaml` (`.sops.yaml:1-19`) | NEW: recipient-set rotation and sealed value identity |
| Bootstrap secret injection | imperative prereqs (`RUNBOOK.md:42-73`) | NEW: root-of-trust bootstrapping |
| External DNS via cove/infra | notes/runbook (`notes/04-dns-tls.md:7-18`) | covered today if imported as Cloudflare provider; PUSH INVALIDATION for cross-repo changes |
| cert-manager ACME HTTP-01 | ClusterIssuers (`clusters/prod-internal/manifests/cert-manager/clusterissuers.yaml:1-35`) | STANDING-DEMAND plus TEMPORAL renewal policy |
| Tailscale tailnet ingress | bootstrap operator and ProxyClass (`clusters/prod-internal/bootstrap/tailscale-operator.yaml:1-30`) | STANDING RUNS and secret custody |
| CoreDNS partial ownership | Corefile-only SSA (`clusters/prod-internal/manifests/coredns/coredns-cm.yaml:1-40`) | FIELD-LEVEL CUSTODY |
| CNPG operator | Argo app (`clusters/prod-internal/apps/cnpg-operator.yaml:1-26`) | STANDING-DEMAND controller as vix function |
| Postgres clusters | CNPG Cluster (`clusters/prod-internal/manifests/forgejo-db/cluster.yaml:1-21`) | covered today: addressed stateful effects, probes |
| WAL/base backups | ObjectStore and ScheduledBackup (`clusters/prod-internal/manifests/forgejo-db/objectstore.yaml:1-21`) | TEMPORAL policy |
| Host restic backups | script/timer (`host/restic/restic-backup.sh:1-44`) | TEMPORAL policy plus NEW restic password custody |
| Restore/adoption | runbook recovery (`RUNBOOK.md:88-121`) | covered today: import/adoption plus temporal quiesce policy |
| OCI image deployment | kustomize image tag bump (`clusters/prod-internal/manifests/app-vixen/kustomization.yaml:17-22`) | covered today by artifact intelligence |
| Runtime readiness/liveness | app probes (`clusters/prod-internal/manifests/app-vixen/deployment.yaml:118-127`) | STANDING RUNS |
| Ingress routing | app/Forgejo/CCC ingresses (`clusters/prod-internal/manifests/app-vixen/ingress.yaml:1-29`) | covered today: addressed route effects |
| Shared OIDC gate | oauth2-proxy runbook (`RUNBOOK.md:227-260`) | NEW for session/cookie secret rotation; otherwise addressed effects |
| RBAC/service accounts | autoscaler RBAC (`clusters/prod-internal/manifests/hcloud-ci-node-autoscaler/rbac.yaml:1-97`) | covered today: tenant claim policy |
| ARC runner scale sets | ARC apps (`clusters/prod-internal/apps/github-actions-runners-bearcove-dedicated.yaml:1-183`) | Rodin-as-scheduler plus STANDING RUNS |
| Node taints/affinity | runner notes/apps (`notes/18-github-oss-runners.md:58-120`) | Rodin-as-scheduler |
| Hetzner Cloud autoscaling | autoscaler app/deployment (`clusters/prod-internal/manifests/hcloud-ci-node-autoscaler/deployment.yaml:1-67`) | LEASES/LINEARIZATION plus provider capability |
| Kubernetes Lease use | autoscaler RBAC grants leases (`clusters/prod-internal/manifests/hcloud-ci-node-autoscaler/rbac.yaml:51-57`) | LEASES/LINEARIZATION |
| Observability stack | `STATE.md` monitoring inventory (`STATE.md:250-262`) | PUSH INVALIDATION inlet and standing roots |
| Alerts/status | Gatus/Prometheus/Better Stack (`STATE.md:250-262`, `ansible/README.md:260-271`) | PUSH INVALIDATION inlet plus TEMPORAL policy |

## NEW Findings

1. Secret management is not just a normal addressed effect. The real repo needs
   first-class sealed values, recipient-set policy, reveal points, rotation
   plans, and root private-key custody. Evidence: SOPS/age creation rules,
   KSOPS repo-server decrypt, bootstrap-only age/deploy/Tailscale secrets,
   Stripe/GitHub/Brevo/Scaleway/restic credentials
   (`.sops.yaml:1-19`, `clusters/prod-internal/bootstrap/argocd.yaml:37-73`,
   `RUNBOOK.md:9-25`, `RUNBOOK.md:42-73`).
2. Secret rotation is temporal and authority-scoped. Examples: oauth2-proxy
   cookie-secret rotation revokes sessions, Stripe webhook secret rotation is
   endpoint-bound, registry PATs are copied across namespaces and must rotate
   together (`RUNBOOK.md:179-183`, `RUNBOOK.md:217-225`,
   `clusters/prod-internal/manifests/bee-site/bee-site-registry.sops.yaml:1-13`).
3. Bootstrap roots are capabilities that must exist before demand evaluation can
   fetch/decrypt its own graph. This is distinct from ordinary provider
   capabilities because losing the root key makes the encrypted journal slice
   unrecoverable (`RUNBOOK.md:9-25`, `notes/08-secrets-bootstrap.md:7-43`).
4. Off-repo authority custody needs an explicit representation. DNS is managed
   in `cove/infra` by another account boundary, while this repo consumes the
   resulting hostnames and certificates (`notes/04-dns-tls.md:7-18`,
   `RUNBOOK.md:248-249`). The provider effect itself is covered by addressed
   effects, but the cross-repo/account custody and push invalidation are not
   first-class yet.
