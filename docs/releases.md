# Self-hosted beta releases

## 0.2.0-beta.3

New in this beta:

- Connections for GitLab (gitlab.com and self-managed, including nested groups),
  Bitbucket Cloud, Bitbucket Data Center and Gitea, alongside GitHub and Forgejo.
  See [code hosts](../README.md#code-hosts) for tokens, repository formats and webhooks.
- A release channel next to the update policy. The stable channel keeps
  pre-releases out of automatic and offered updates; the beta channel includes them.
- The plan catalog follows the current price list: Freelancer EUR 5.99/month,
  Teams EUR 9.99 per user/month for up to 10 users with 20 automations each,
  and Enterprise by arrangement. A free Teams beta now allows 10 users and 200
  automations. Licences signed under the earlier prices keep their allowance.
- The Cloud preview lists managed AI at twice Anthropic's list price, in EUR,
  and explains that your own provider key is the cheaper, recommended option.
- The app uses the current Diffrook logo and favicon.
- Refreshed TLS dependencies (rustls, rustls-webpki, webpki-roots) for recent
  security advisories.

Bitbucket Data Center has no issue tracker, so only pull request automations
apply there. Comment commands need an allowed-actors list on every host except GitHub.

This is a self-hosted beta for pilot installations. Cloud Hosting is Coming soon:
there is no hosted deployment, Cloud activation, checkout or token billing.
Customers provide the server and AI endpoint. Individual, Freelancer and Teams
are free during the beta program; Enterprise is excluded and unavailable.

- Individual: personal noncommercial use, 1 user and 3 automations, free forever.
- Freelancer: professional and commercial use, 1 user and 10 automations, no SSO, free during beta.
- Teams: organizational and commercial use, up to 10 users and 20 automations per user, OIDC SSO, free during beta.
- Enterprise: excluded from free beta; new paid sales remain disabled.

Select the free plan in Settings. SSO-only installations can bootstrap with
`DIFFROOK_BETA_PLAN=teams`. No payment, card or signed beta file is required.
The first official stable release ends the beta program. Stable builds require
paid signed licenses for Freelancer and Teams and ignore stored beta selections.
Individual remains free forever. No automatic subscription or charge is created.
Review [licensing](licensing.md) before upgrading to the official release.

All registered users are administrators. Do not assign SSO access to users who
should have restricted permissions. Existing identities can still sign in after
expiry or downgrade; new SSO identities require Teams or Enterprise. Legacy
Business licenses retain their original allowances and expiry.

## New Docker installation

Install Git and Docker with Compose on the customer's server, then:

```sh
git clone --branch v0.2.0-beta.3 --depth 1 https://github.com/Alexandre1116/diffrook.git
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
   continuing professional or company use of a stable release. During this beta,
   select the free Freelancer or Teams entitlement in Settings after upgrading.
4. Check out this tag and rebuild the container. With a clean source checkout:

```sh
git fetch --tags
git checkout v0.2.0-beta.3
docker compose up --build -d
```

Preserve the existing Compose project name, data volume and instance configuration.
Linux x86-64 and ARM64 installations may also explicitly install the matching
package from the dashboard. The update archives keep license notices under
`web/licenses/`, which older extractors accept. Archive checksum files are
included in the GitHub release, and in-app updates verify GitHub's asset digest.
From this version onward, Automatic updates only installs stable versions
unless the dashboard's release channel is set to Beta releases. On the stable
channel, pre-releases are not offered; switch to the beta channel to install them.

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
