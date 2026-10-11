# Self-hosted beta releases

## 0.2.0-beta.1

This is a self-hosted beta for pilot installations. Cloud Hosting is Coming soon:
there is no hosted deployment, Cloud activation, checkout or token billing.
Customers provide the server and AI endpoint. Professional and organizational
use requires a paid signed license; payment and renewal are arranged manually.

- Individual: personal noncommercial use, 1 user and 3 automations, free forever.
- Freelancer: 1 user and 10 automations, local sign-in, EUR 9/month or EUR 90/year.
- Teams: 5 users and 20 automations, OIDC SSO, EUR 29/month or EUR 290/year.
- Enterprise: at least 10 users, 10 automations per user pooled, OIDC SSO,
  EUR 8/user/month or EUR 80/user/year, with optional automation packs.

Prices exclude applicable tax. See [licensing](licensing.md) for exact terms.
All registered users are administrators. Do not assign SSO access to users who
should have restricted permissions. Existing identities can still sign in after
expiry or downgrade; new SSO identities require Teams or Enterprise. Legacy
Business licenses retain their original allowances and expiry.

## New Docker installation

Install Git and Docker with Compose on the customer's server, then:

```sh
git clone --branch v0.2.0-beta.1 --depth 1 https://github.com/Alexandre1116/diffrook.git
cd diffrook
docker compose up --build -d
docker compose logs diffrook
```

Open http://localhost:8080 and use the setup token in the logs. This release is
built from its tagged source; a prebuilt registry image is not published.
The default port is bound to loopback. For remote access configure the customer's
own public URL, HTTPS proxy and IdP as described in [SSO and Internet deployment](sso-and-internet.md).
Do not expose the default HTTP listener directly to the Internet.

## Upgrading an existing installation

1. Choose Manual updates before upgrading. Releases before 0.2 can automatically
   select pre-releases; the new stable-only policy cannot change an older binary.
2. Stop Diffrook and back up the complete data volume, including the encryption
   key. Follow the backup instructions in [README](../README.md#storage-and-backups).
   Confirm that the backup can be restored to an isolated empty volume.
3. Review the new license and quotas. Existing over-quota data is retained, but
   creating more users or automations is blocked. Obtain a paid license before
   continuing professional or company use of this version.
4. Check out this tag and rebuild the container. With a clean source checkout:

```sh
git fetch --tags
git checkout v0.2.0-beta.1
docker compose up --build -d
```

Preserve the existing Compose project name, data volume and instance configuration.
Linux x86-64 and ARM64 installations may also explicitly install the matching
package from the dashboard. The update archives keep license notices under
`web/licenses/`, which older extractors accept. Archive checksum files are
included in the GitHub release, and in-app updates verify GitHub's asset digest.
From this version onward, Automatic updates only installs stable versions;
pre-releases remain available through explicit manual installation.

Validate login, company IdP allowlists, a representative automation and webhook
before enabling production repositories. Rollback to an older image after a
failed deployment must also restore its stopped-volume backup if the newer
version changed persisted data. Startup rollback alone is not a database restore.

## Publishing

Run CI on the exact commit. Create a matching tag and a draft pre-release, then
run the CI workflow manually with `release_tag` set to that tag. Both Linux
architectures must build successfully, pass the packaged-binary integration
checks and restore a complete-volume backup before their archives and SHA-256
files are uploaded. Publish the draft only after all checks pass. Publishing does not rebuild or
overwrite validated assets.

The beta does not claim a penetration-test certificate, an availability SLA or
validation with every IdP. The scoped RSA dependency advisory and its applicability
are documented in [security verification](sso-and-internet.md#verification-and-scope).
