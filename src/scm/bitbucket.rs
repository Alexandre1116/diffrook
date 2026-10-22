//! Bitbucket Cloud (REST 2.0, `workspace/repo`) and Bitbucket Data Center
//! (REST 1.0, `PROJECT/repo`). Data Center has no issue tracker; teams there
//! use Jira, so issue operations are rejected with a clear error.
use super::*;

fn cloud(scm: &Scm) -> bool {
    scm.kind == Kind::BitbucketCloud
}

/// `/repositories/{workspace}/{slug}` or `/projects/{key}/repos/{slug}`.
fn repo_base(scm: &Scm, repo: &str) -> Result<String> {
    let (owner, name) = repo
        .split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
        .ok_or_else(|| anyhow!("Bitbucket repository must be owner/name"))?;
    Ok(if cloud(scm) {
        format!("/repositories/{}/{}", segment(owner)?, segment(name)?)
    } else {
        format!("/projects/{}/repos/{}", segment(owner)?, segment(name)?)
    })
}

fn no_issues() -> anyhow::Error {
    anyhow!("Bitbucket Data Center has no built-in issue tracker; use pull request automations")
}

/// Cloud users are identified by nickname; Data Center by user slug/name.
fn user_name(v: &Value) -> String {
    ["nickname", "name", "slug", "display_name"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

fn dc_link(v: &Value) -> String {
    v.pointer("/links/self/0/href")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Cloud pages carry an absolute `next` link.
async fn cloud_all(scm: &Scm, path: &str, max_pages: u32) -> Result<Vec<Value>> {
    let mut url = scm.url(path);
    let mut rows = Vec::new();
    for _ in 0..max_pages {
        let page = scm.get_absolute(&url).await?;
        rows.extend(page["values"].as_array().cloned().unwrap_or_default());
        match page["next"].as_str() {
            Some(next) => url = next.to_owned(),
            None => return Ok(rows),
        }
    }
    bail!("Bitbucket pagination exceeded {max_pages} pages")
}

/// Data Center pages use start/limit with isLastPage and nextPageStart.
async fn dc_all(scm: &Scm, path: &str, max_pages: u32) -> Result<Vec<Value>> {
    let sep = if path.contains('?') { '&' } else { '?' };
    let mut start = 0;
    let mut rows = Vec::new();
    for _ in 0..max_pages {
        let page = scm
            .get(&format!("{path}{sep}limit=100&start={start}"))
            .await?;
        rows.extend(page["values"].as_array().cloned().unwrap_or_default());
        match page["nextPageStart"].as_u64() {
            Some(next) if page["isLastPage"] == false => start = next,
            _ => return Ok(rows),
        }
    }
    bail!("Bitbucket pagination exceeded {max_pages} pages")
}

pub(super) async fn identity(scm: &Scm) -> Result<String> {
    if cloud(scm) {
        let response = scm
            .request(reqwest::Method::GET, "/user")
            .send()
            .await
            .context("testing Bitbucket connection")?;
        let status = response.status();
        // Workspace and repository access tokens authenticate but cannot read
        // /user; a 403 still proves the token itself was accepted.
        if status == reqwest::StatusCode::FORBIDDEN {
            return Ok("a Bitbucket access token".into());
        }
        let user = scm.json(response).await?;
        return Ok(user_name(&user));
    }
    let response = scm
        .request(reqwest::Method::GET, "/profile/recent/repos?limit=1")
        .send()
        .await
        .context("testing Bitbucket connection")?;
    let status = response.status();
    if !status.is_success() {
        bail!("SCM returned HTTP {status}")
    }
    Ok(response
        .headers()
        .get("x-ausername")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("user")
        .to_owned())
}

pub(super) async fn repository(scm: &Scm, repo: &str) -> Result<Repository> {
    let base = repo_base(scm, repo)?;
    let v = scm.get(&base).await?;
    if cloud(scm) {
        return Ok(Repository {
            full_name: v["full_name"].as_str().unwrap_or(repo).to_owned(),
            default_branch: v
                .pointer("/mainbranch/name")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_owned(),
            html_url: v
                .pointer("/links/html/href")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            fork: v["parent"].is_object(),
        });
    }
    let default_branch = scm
        .get(&format!("{base}/default-branch"))
        .await
        .ok()
        .and_then(|b| b["displayId"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "main".into());
    Ok(Repository {
        full_name: repo.to_owned(),
        default_branch,
        html_url: dc_link(&v),
        fork: v["origin"].is_object(),
    })
}

/// Bitbucket Cloud has no recursive tree call, so walk directories breadth
/// first. Data Center lists every file path in one paged call.
pub(super) async fn tree_at(scm: &Scm, repo: &str, sha: &str) -> Result<Vec<TreeEntry>> {
    let base = repo_base(scm, repo)?;
    let mut entries = Vec::new();
    if !cloud(scm) {
        for path in dc_all(scm, &format!("{base}/files?at={}", segment(sha)?), 1000).await? {
            let path = path.as_str().unwrap_or_default().to_owned();
            validate_path(&path)?;
            entries.push(TreeEntry {
                path,
                sha: String::new(),
                size: None,
                kind: "blob".into(),
                mode: "100644".into(),
            });
        }
        return Ok(entries);
    }
    let mut directories = std::collections::VecDeque::from([String::new()]);
    let mut visited = 0;
    while let Some(dir) = directories.pop_front() {
        visited += 1;
        if visited > 20_000 {
            bail!("Bitbucket tree exceeds 20000 directories; refusing incomplete coverage")
        }
        let encoded_dir = if dir.is_empty() {
            String::new()
        } else {
            format!(
                "{}/",
                dir.split('/')
                    .map(segment)
                    .collect::<Result<Vec<_>>>()?
                    .join("/")
            )
        };
        let rows = cloud_all(
            scm,
            &format!("{base}/src/{}/{encoded_dir}?pagelen=100", segment(sha)?),
            1000,
        )
        .await?;
        for row in rows {
            let path = str_at(&row, "path");
            validate_path(&path)?;
            if row["type"] == "commit_directory" {
                directories.push_back(path.clone());
                entries.push(TreeEntry {
                    path,
                    sha: String::new(),
                    size: None,
                    kind: "tree".into(),
                    mode: "040000".into(),
                });
                continue;
            }
            let attributes = row["attributes"].as_array().cloned().unwrap_or_default();
            let has = |a: &str| attributes.iter().any(|x| x == a);
            let (kind, mode) = if has("subrepository") {
                ("commit", "160000")
            } else if has("link") {
                ("blob", "120000")
            } else if has("executable") {
                ("blob", "100755")
            } else {
                ("blob", "100644")
            };
            entries.push(TreeEntry {
                path,
                sha: String::new(),
                size: row["size"].as_u64(),
                kind: kind.into(),
                mode: mode.into(),
            });
        }
    }
    Ok(entries)
}

pub(super) async fn file(
    scm: &Scm,
    repo: &str,
    path: &str,
    reference: &str,
) -> Result<FileContent> {
    let base = repo_base(scm, repo)?;
    let encoded = path
        .split('/')
        .map(segment)
        .collect::<Result<Vec<_>>>()?
        .join("/");
    let content = if cloud(scm) {
        // Branch names may contain '/', which is ambiguous in /src/{ref}/{path}.
        let sha = if is_full_sha(reference) {
            reference.to_owned()
        } else {
            branch_sha(scm, repo, reference).await?
        };
        scm.get_text(&format!("{base}/src/{sha}/{encoded}"), 32 * 1024 * 1024)
            .await?
    } else {
        scm.get_text(
            &format!("{base}/raw/{encoded}?at={}", escape_all(reference)),
            32 * 1024 * 1024,
        )
        .await?
    };
    Ok(FileContent {
        path: path.into(),
        size: content.len(),
        content,
        sha: String::new(),
    })
}

/// Cloud pull request payloads carry abbreviated hashes; the engine compares
/// them with full branch heads, so expand them first.
async fn full_hash(scm: &Scm, repo: &str, short: &str) -> Result<String> {
    if short.is_empty() || is_full_sha(short) {
        return Ok(short.to_owned());
    }
    let commit = scm
        .get(&format!(
            "{}/commit/{}",
            repo_base(scm, repo)?,
            segment(short)?
        ))
        .await?;
    Ok(str_at(&commit, "hash"))
}

pub(super) async fn pull_request(scm: &Scm, repo: &str, number: u64) -> Result<PullRequest> {
    let base = repo_base(scm, repo)?;
    if cloud(scm) {
        let data = scm.get(&format!("{base}/pullrequests/{number}")).await?;
        let diff = scm
            .get_text(
                &format!("{base}/pullrequests/{number}/diff"),
                16 * 1024 * 1024,
            )
            .await?;
        let same_repo =
            data.pointer("/source/repository/uuid") == data.pointer("/destination/repository/uuid");
        let head_repo = if same_repo {
            repo.to_owned()
        } else {
            data.pointer("/source/repository/full_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let text = |p: &str| {
            data.pointer(p)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let head_sha = full_hash(
            scm,
            if same_repo { repo } else { &head_repo },
            &text("/source/commit/hash"),
        )
        .await?;
        return Ok(PullRequest {
            number,
            title: text("/title"),
            body: text("/description"),
            head_ref: text("/source/branch/name"),
            head_sha,
            base_ref: text("/destination/branch/name"),
            base_sha: full_hash(scm, repo, &text("/destination/commit/hash")).await?,
            head_repo,
            author: user_name(&data["author"]),
            draft: data["draft"] == true,
            html_url: text("/links/html/href"),
            diff,
        });
    }
    let data = scm.get(&format!("{base}/pull-requests/{number}")).await?;
    let diff = scm
        .get_text(
            &format!("{base}/pull-requests/{number}.diff"),
            16 * 1024 * 1024,
        )
        .await?;
    let head_repo =
        if data.pointer("/fromRef/repository/id") == data.pointer("/toRef/repository/id") {
            repo.to_owned()
        } else {
            format!(
                "{}/{}",
                data.pointer("/fromRef/repository/project/key")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                data.pointer("/fromRef/repository/slug")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            )
        };
    let text = |p: &str| {
        data.pointer(p)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Ok(PullRequest {
        number,
        title: text("/title"),
        body: text("/description"),
        head_ref: text("/fromRef/displayId"),
        head_sha: text("/fromRef/latestCommit"),
        head_repo,
        base_ref: text("/toRef/displayId"),
        base_sha: text("/toRef/latestCommit"),
        author: user_name(&data["author"]["user"]),
        draft: data["draft"] == true,
        html_url: dc_link(&data),
        diff,
    })
}

/// Cloud issues have no labels; expose kind and component so label filters
/// can still select them.
pub fn issue_labels(issue: &Value) -> Vec<String> {
    [issue["kind"].as_str(), issue["component"]["name"].as_str()]
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect()
}

pub(super) async fn issue(scm: &Scm, repo: &str, number: u64) -> Result<Issue> {
    if !cloud(scm) {
        return Err(no_issues());
    }
    let base = format!("{}/issues/{number}", repo_base(scm, repo)?);
    let data = scm.get(&base).await?;
    let comments = cloud_all(scm, &format!("{base}/comments?pagelen=100"), 10).await?;
    Ok(Issue {
        number,
        title: str_at(&data, "title"),
        body: data
            .pointer("/content/raw")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        author: user_name(&data["reporter"]),
        labels: issue_labels(&data),
        comments: comments
            .iter()
            .filter_map(|c| {
                let text = c.pointer("/content/raw").and_then(Value::as_str)?;
                Some(format!("{}: {text}", user_name(&c["user"])))
            })
            .collect(),
        html_url: data
            .pointer("/links/html/href")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
    })
}

pub(super) async fn open_pull_requests(scm: &Scm, repo: &str) -> Result<Vec<Value>> {
    let base = repo_base(scm, repo)?;
    if cloud(scm) {
        let rows = cloud_all(
            scm,
            &format!("{base}/pullrequests?state=OPEN&pagelen=50"),
            1000,
        )
        .await?;
        return Ok(rows
            .iter()
            .map(|r| {
                json!({"number": r["id"], "user": {"login": user_name(&r["author"])}, "draft": r["draft"] == true,
                    "labels": [], "head": {"ref": r.pointer("/source/branch/name")}})
            })
            .collect());
    }
    let rows = dc_all(scm, &format!("{base}/pull-requests?state=OPEN"), 1000).await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({"number": r["id"], "user": {"login": user_name(&r["author"]["user"])}, "draft": r["draft"] == true,
                "labels": [], "head": {"ref": r.pointer("/fromRef/displayId")}})
        })
        .collect())
}

pub(super) async fn open_issues(scm: &Scm, repo: &str) -> Result<Vec<Value>> {
    if !cloud(scm) {
        return Err(no_issues());
    }
    let query = escape_all(r#"state="new" OR state="open""#);
    let rows = cloud_all(
        scm,
        &format!("{}/issues?q={query}&pagelen=50", repo_base(scm, repo)?),
        1000,
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({"number": r["id"], "user": {"login": user_name(&r["reporter"])},
                "labels": issue_labels(r).into_iter().map(|n| json!({"name": n})).collect::<Vec<_>>()})
        })
        .collect())
}

pub(super) async fn post_comment(
    scm: &Scm,
    repo: &str,
    is_pull_request: bool,
    number: u64,
    body: &str,
) -> Result<()> {
    let base = repo_base(scm, repo)?;
    let request = if cloud(scm) {
        let kind = if is_pull_request {
            "pullrequests"
        } else {
            "issues"
        };
        scm.request(
            reqwest::Method::POST,
            &format!("{base}/{kind}/{number}/comments"),
        )
        .json(&json!({"content": {"raw": body}}))
    } else if is_pull_request {
        scm.request(
            reqwest::Method::POST,
            &format!("{base}/pull-requests/{number}/comments"),
        )
        .json(&json!({"text": body}))
    } else {
        return Err(no_issues());
    };
    scm.send(request, "posting Bitbucket comment").await?;
    Ok(())
}

pub(super) async fn branch_sha(scm: &Scm, repo: &str, name: &str) -> Result<String> {
    let base = repo_base(scm, repo)?;
    if cloud(scm) {
        let v = scm
            .get(&format!("{base}/refs/branches/{}", branch_path(name)?))
            .await?;
        return Ok(v
            .pointer("/target/hash")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("branch response has no sha"))?
            .into());
    }
    branch_path(name)?;
    // filterText is a substring match; pick the exact branch.
    let branches = dc_all(
        scm,
        &format!("{base}/branches?filterText={}", escape_all(name)),
        20,
    )
    .await?;
    branches
        .iter()
        .find(|b| b["displayId"] == name)
        .and_then(|b| b["latestCommit"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("branch {name} not found"))
}

pub(super) async fn create_branch(scm: &Scm, repo: &str, name: &str, sha: &str) -> Result<()> {
    branch_path(name)?;
    let base = repo_base(scm, repo)?;
    let request = if cloud(scm) {
        scm.request(reqwest::Method::POST, &format!("{base}/refs/branches"))
            .json(&json!({"name": name, "target": {"hash": sha}}))
    } else {
        scm.request(reqwest::Method::POST, &format!("{base}/branches"))
            .json(&json!({"name": name, "startPoint": sha}))
    };
    scm.send(request, "creating Bitbucket branch").await?;
    Ok(())
}

pub(super) async fn commit_changes(
    scm: &Scm,
    repo: &str,
    branch: &str,
    base_sha: &str,
    entries: &[TreeEntry],
    changes: &[FileChange],
    message: &str,
) -> Result<String> {
    let checked = Scm::checked_changes(entries, changes)?;
    let base = repo_base(scm, repo)?;
    if cloud(scm) {
        // One commit for the whole change set. Paths get a leading slash so a
        // file named "message" or "branch" is not read as a form field, and
        // `parents` makes Bitbucket reject the commit if the branch moved.
        let form = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("message", message)
                .append_pair("branch", branch)
                .append_pair("parents", base_sha);
            for (ch, _) in &checked {
                form.append_pair(&format!("/{}", ch.path), &ch.content);
            }
            form.finish()
        };
        let response = scm
            .request(reqwest::Method::POST, &format!("{base}/src"))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form)
            .send()
            .await
            .context("committing Bitbucket files")?;
        let status = response.status();
        if !status.is_success() {
            bail!("SCM returned HTTP {status}");
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let sha = location.rsplit('/').next().unwrap_or_default();
        if is_full_sha(sha) {
            return Ok(sha.to_owned());
        }
        // Older responses omit Location; the branch head is the new commit.
        let head = branch_sha(scm, repo, branch).await?;
        if head == base_sha {
            bail!("Bitbucket did not create a commit")
        }
        return Ok(head);
    }
    // Data Center edits one file per commit; chain each commit on the last so
    // a concurrent push makes the next edit fail instead of being overwritten.
    let mut head = base_sha.to_owned();
    for (ch, current) in checked {
        let path = ch
            .path
            .split('/')
            .map(segment)
            .collect::<Result<Vec<_>>>()?
            .join("/");
        let mut fields = vec![
            ("branch", branch.to_owned()),
            ("content", ch.content.clone()),
            ("message", message.to_owned()),
        ];
        if current.is_some() {
            fields.push(("sourceCommitId", head.clone()));
        }
        let (content_type, body) = multipart(&fields);
        let commit = scm
            .send(
                scm.request(reqwest::Method::PUT, &format!("{base}/browse/{path}"))
                    .header("x-atlassian-token", "no-check")
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(body),
                "committing Bitbucket file",
            )
            .await?;
        head = commit["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Bitbucket response has no commit id"))?
            .to_owned();
    }
    Ok(head)
}

fn multipart(fields: &[(&str, String)]) -> (String, Vec<u8>) {
    let boundary = format!("diffrook-{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

pub(super) async fn create_pull_request(
    scm: &Scm,
    repo: &str,
    title: &str,
    body: &str,
    head: &str,
    base: &str,
) -> Result<Value> {
    let path = repo_base(scm, repo)?;
    if cloud(scm) {
        let pr = scm
            .send(
                scm.request(reqwest::Method::POST, &format!("{path}/pullrequests"))
                    .json(&json!({"title": title, "description": body,
                        "source": {"branch": {"name": head}}, "destination": {"branch": {"name": base}}})),
                "creating Bitbucket pull request",
            )
            .await?;
        return Ok(json!({"number": pr["id"], "html_url": pr.pointer("/links/html/href")}));
    }
    let (project, slug) = repo.split_once('/').unwrap_or_default();
    let target = json!({"slug": slug, "project": {"key": project}});
    let pr = scm
        .send(
            scm.request(reqwest::Method::POST, &format!("{path}/pull-requests"))
                .json(&json!({"title": title, "description": body,
                    "fromRef": {"id": format!("refs/heads/{head}"), "repository": target},
                    "toRef": {"id": format!("refs/heads/{base}"), "repository": target}})),
            "creating Bitbucket pull request",
        )
        .await?;
    Ok(json!({"number": pr["id"], "html_url": dc_link(&pr)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scm::test_server::{client, ok, serve, Reply};

    fn change(path: &str, content: &str) -> FileChange {
        FileChange {
            path: path.into(),
            content: content.into(),
        }
    }
    fn scm(kind: &str, origin: &str) -> Scm {
        Scm::from_connection(
            &json!({"kind":kind,"base_url":origin,"token":"bb-test-token"}),
            &client(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn cloud_expands_short_hashes_follows_diff_redirect_and_commits_once() {
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let r = "/2.0/repositories/acme/api";
        let (origin, server) = serve(vec![
            ok(&json!({"title":"Divide","description":"d","draft":false,"author":{"nickname":"dev"},
                "source":{"branch":{"name":"feature"},"commit":{"hash":"aaaaaaaaaaaa"},"repository":{"uuid":"{1}","full_name":"acme/api"}},
                "destination":{"branch":{"name":"main"},"commit":{"hash":"bbbbbbbbbbbb"},"repository":{"uuid":"{1}"}},
                "links":{"html":{"href":"https://bitbucket.org/acme/api/pull-requests/4"}}}).to_string()),
            Reply { status: "302 Found", headers: vec![("location", format!("{{origin}}{r}/diff/acme/api:aaaaaaaaaaaa"))], body: String::new() },
            ok("diff --git a/src/main.rs b/src/main.rs\n"),
            ok(&json!({"hash":a}).to_string()),
            ok(&json!({"hash":b}).to_string()),
            ok(&json!({"target":{"hash":a}}).to_string()),
            ok(&json!({"values":[{"type":"commit_directory","path":"src"}]}).to_string()),
            ok(&json!({"values":[{"type":"commit_file","path":"src/main.rs","size":12,"attributes":[]}],
                "next":format!("{{origin}}{r}/src/{a}/src/?pagelen=100&page=2")}).to_string()),
            ok(&json!({"values":[{"type":"commit_file","path":"src/run.sh","attributes":["executable"]}]}).to_string()),
            Reply { status: "201 Created", headers: vec![("location", format!("{{origin}}{r}/commit/{c}"))], body: String::new() },
        ])
        .await;
        let scm = scm("bitbucket", &origin);
        let pr = scm.pull_request("acme/api", 4).await.unwrap();
        assert_eq!(
            (pr.head_sha.as_str(), pr.base_sha.as_str()),
            (a.as_str(), b.as_str())
        );
        assert_eq!(
            (pr.head_repo.as_str(), pr.author.as_str()),
            ("acme/api", "dev")
        );
        assert!(pr.diff.starts_with("diff --git"));
        let sha = scm
            .commit_changes(
                "acme/api",
                "feature",
                &a,
                &[change("src/main.rs", "fn main() {}")],
                "fix",
            )
            .await
            .unwrap();
        assert_eq!(sha, c);

        let seen = server.await.unwrap();
        let paths: Vec<_> = seen
            .iter()
            .map(|s| format!("{} {}", s.method, s.path))
            .collect();
        assert_eq!(
            paths,
            [
                format!("GET {r}/pullrequests/4"),
                format!("GET {r}/pullrequests/4/diff"),
                format!("GET {r}/diff/acme/api:aaaaaaaaaaaa"),
                format!("GET {r}/commit/aaaaaaaaaaaa"),
                format!("GET {r}/commit/bbbbbbbbbbbb"),
                format!("GET {r}/refs/branches/feature"),
                format!("GET {r}/src/{a}/?pagelen=100"),
                format!("GET {r}/src/{a}/src/?pagelen=100"),
                format!("GET {r}/src/{a}/src/?pagelen=100&page=2"),
                format!("POST {r}/src"),
            ]
        );
        assert!(seen
            .iter()
            .all(|s| s.headers.contains("authorization: bearer bb-test-token")));
        let form: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(seen[9].body.as_bytes())
                .into_owned()
                .collect();
        assert_eq!(form["message"], "fix");
        assert_eq!(form["branch"], "feature");
        assert_eq!(form["parents"], a);
        assert_eq!(form["/src/main.rs"], "fn main() {}");
    }

    #[tokio::test]
    async fn cloud_basic_auth_and_foreign_links_are_refused() {
        let (origin, server) = serve(vec![ok(
            &json!({"values":[],"next":"https://elsewhere.example/2.0/repositories/acme/api/pullrequests?page=2"}).to_string(),
        )])
        .await;
        let scm = Scm::from_connection(
            &json!({"kind":"bitbucket","base_url":origin,"token":"dev@example.com:api-token"}),
            &client(),
        )
        .unwrap();
        let err = scm.open_pull_requests("acme/api").await.unwrap_err();
        assert!(err.to_string().contains("outside the configured API"));
        let seen = server.await.unwrap();
        assert!(seen[0].headers.contains("authorization: basic "));
    }

    #[tokio::test]
    async fn data_center_matches_exact_branch_and_chains_file_commits() {
        let (a, c, d) = ("a".repeat(40), "c".repeat(40), "d".repeat(40));
        let r = "/rest/api/latest/projects/ACME/repos/api";
        let branches = json!({"isLastPage":true,"values":[
            {"displayId":"feature/x-old","latestCommit":"0".repeat(40)},
            {"displayId":"feature/x","latestCommit":a}]})
        .to_string();
        let (origin, server) = serve(vec![
            ok(&branches),
            ok(&json!({"isLastPage":true,"values":["src/main.rs"]}).to_string()),
            ok(&json!({"id":c}).to_string()),
            ok(&json!({"id":d}).to_string()),
            ok(
                &json!({"id":5,"links":{"self":[{"href":"https://bitbucket.example/pr/5"}]}})
                    .to_string(),
            ),
        ])
        .await;
        let scm = scm("bitbucket_datacenter", &origin);
        let sha = scm
            .commit_changes(
                "ACME/api",
                "feature/x",
                &a,
                &[
                    change("src/main.rs", "fn main() {}"),
                    change("docs/new.md", "# New"),
                ],
                "fix",
            )
            .await
            .unwrap();
        assert_eq!(sha, d);
        let pr = scm
            .create_pull_request("ACME/api", "t", "b", "diffrook/run-1", "feature/x")
            .await
            .unwrap();
        assert_eq!(
            pr,
            json!({"number":5,"html_url":"https://bitbucket.example/pr/5"})
        );
        assert!(scm
            .issue("ACME/api", 1)
            .await
            .unwrap_err()
            .to_string()
            .contains("no built-in issue tracker"));

        let seen = server.await.unwrap();
        let paths: Vec<_> = seen
            .iter()
            .map(|s| format!("{} {}", s.method, s.path))
            .collect();
        assert_eq!(
            paths,
            [
                format!("GET {r}/branches?filterText=feature%2Fx&limit=100&start=0"),
                format!("GET {r}/files?at={a}&limit=100&start=0"),
                format!("PUT {r}/browse/src/main.rs"),
                format!("PUT {r}/browse/docs/new.md"),
                format!("POST {r}/pull-requests"),
            ]
        );
        assert!(seen[2].headers.contains("x-atlassian-token: no-check"));
        assert!(seen[2]
            .body
            .contains(&format!("name=\"sourceCommitId\"\r\n\r\n{a}\r\n")));
        assert!(seen[2]
            .body
            .contains("name=\"content\"\r\n\r\nfn main() {}\r\n"));
        // A new file has no previous commit to check against.
        assert!(!seen[3].body.contains("sourceCommitId"));
        let body: Value = serde_json::from_str(&seen[4].body).unwrap();
        assert_eq!(body["fromRef"]["id"], "refs/heads/diffrook/run-1");
        assert_eq!(body["toRef"]["repository"]["project"]["key"], "ACME");
    }
}
