//! GitLab REST v4 (gitlab.com and self-managed). Merge requests are exposed
//! as pull requests and addressed by their project-scoped IID.
use super::*;

/// `/projects/:id` where the id is the URL-encoded `group/subgroup/project`.
fn project(repo: &str) -> Result<String> {
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() < 2 || parts.len() > Kind::GitLab.max_repository_segments() {
        bail!("GitLab repository must be group/project")
    }
    let encoded = parts
        .iter()
        .map(|p| segment(p))
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("/projects/{}", encoded.join("%2F")))
}

fn username(v: &Value) -> String {
    v.get("username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Follow GitLab's page-based pagination (the X-Next-Page header).
async fn all(scm: &Scm, path: &str, max_pages: u32) -> Result<Vec<Value>> {
    let sep = if path.contains('?') { '&' } else { '?' };
    let mut rows = Vec::new();
    for page in 1..=max_pages {
        let (data, headers) = scm
            .get_with_headers(&format!("{path}{sep}per_page=100&page={page}"))
            .await?;
        let batch = data
            .as_array()
            .ok_or_else(|| anyhow!("GitLab list response is not an array"))?;
        let empty = batch.is_empty();
        rows.extend(batch.iter().cloned());
        let next = headers
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if next.is_empty() || empty {
            return Ok(rows);
        }
    }
    bail!("GitLab pagination exceeded {max_pages} pages")
}

pub(super) async fn identity(scm: &Scm) -> Result<String> {
    Ok(username(&scm.get("/user").await?))
}

pub(super) async fn repository(scm: &Scm, repo: &str) -> Result<Repository> {
    let v = scm.get(&project(repo)?).await?;
    Ok(Repository {
        full_name: v
            .get("path_with_namespace")
            .and_then(Value::as_str)
            .unwrap_or(repo)
            .to_owned(),
        default_branch: v
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_owned(),
        html_url: str_at(&v, "web_url"),
        fork: v.get("forked_from_project").is_some_and(Value::is_object),
    })
}

pub(super) async fn tree_at(scm: &Scm, repo: &str, sha: &str) -> Result<Vec<TreeEntry>> {
    let rows = all(
        scm,
        &format!(
            "{}/repository/tree?ref={}&recursive=true",
            project(repo)?,
            segment(sha)?
        ),
        1000,
    )
    .await?;
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let path = str_at(&row, "path");
        validate_path(&path)?;
        entries.push(TreeEntry {
            path,
            sha: str_at(&row, "id"),
            size: None,
            kind: row
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("blob")
                .to_owned(),
            mode: row
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("100644")
                .to_owned(),
        });
    }
    Ok(entries)
}

pub(super) async fn file(
    scm: &Scm,
    repo: &str,
    path: &str,
    reference: &str,
) -> Result<FileContent> {
    let data = scm
        .get(&format!(
            "{}/repository/files/{}?ref={}",
            project(repo)?,
            escape_all(path),
            escape_all(reference)
        ))
        .await?;
    let content = decode_utf8_base64(
        data.get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("SCM file response has no content"))?,
    )?;
    Ok(FileContent {
        path: path.into(),
        size: content.len(),
        content,
        sha: str_at(&data, "blob_id"),
    })
}

/// GitLab returns per-file hunks; rebuild a unified diff like GitHub's.
fn unified_diff(files: &[Value]) -> String {
    let mut out = String::new();
    for f in files {
        let old = str_at(f, "old_path");
        let new = str_at(f, "new_path");
        out.push_str(&format!("diff --git a/{old} b/{new}\n"));
        if f["new_file"] == true {
            out.push_str("--- /dev/null\n");
        } else {
            out.push_str(&format!("--- a/{old}\n"));
        }
        if f["deleted_file"] == true {
            out.push_str("+++ /dev/null\n");
        } else {
            out.push_str(&format!("+++ b/{new}\n"));
        }
        let hunk = str_at(f, "diff");
        out.push_str(&hunk);
        if !hunk.is_empty() && !hunk.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

pub(super) async fn pull_request(scm: &Scm, repo: &str, number: u64) -> Result<PullRequest> {
    let base = format!("{}/merge_requests/{number}", project(repo)?);
    let data = scm.get(&base).await?;
    let files = all(scm, &format!("{base}/diffs"), 100).await?;
    // Same project means same repository; only forks need a lookup.
    let head_repo = if data["source_project_id"] == data["target_project_id"] {
        repo.to_owned()
    } else {
        let id = data["source_project_id"]
            .as_u64()
            .ok_or_else(|| anyhow!("merge request has no source project"))?;
        str_at(
            &scm.get(&format!("/projects/{id}")).await?,
            "path_with_namespace",
        )
    };
    Ok(PullRequest {
        number,
        title: str_at(&data, "title"),
        body: str_at(&data, "description"),
        head_ref: str_at(&data, "source_branch"),
        head_sha: str_at(&data, "sha"),
        head_repo,
        base_ref: str_at(&data, "target_branch"),
        base_sha: data
            .pointer("/diff_refs/base_sha")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        author: username(&data["author"]),
        draft: data["draft"] == true || data["work_in_progress"] == true,
        html_url: str_at(&data, "web_url"),
        diff: unified_diff(&files),
    })
}

pub(super) async fn issue(scm: &Scm, repo: &str, number: u64) -> Result<Issue> {
    let base = format!("{}/issues/{number}", project(repo)?);
    let data = scm.get(&base).await?;
    let notes = all(scm, &format!("{base}/notes?sort=asc"), 10).await?;
    Ok(Issue {
        number,
        title: str_at(&data, "title"),
        body: str_at(&data, "description"),
        author: username(&data["author"]),
        labels: label_names(&data["labels"]),
        comments: notes
            .iter()
            .filter(|n| n["system"] != true)
            .map(|n| format!("{}: {}", username(&n["author"]), str_at(n, "body")))
            .collect(),
        html_url: str_at(&data, "web_url"),
    })
}

/// Labels arrive as plain strings in the REST API and as {title} in webhooks.
pub fn label_names(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().or_else(|| l["title"].as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) async fn open_items(scm: &Scm, repo: &str, merge_requests: bool) -> Result<Vec<Value>> {
    let path = if merge_requests {
        "merge_requests"
    } else {
        "issues"
    };
    let rows = all(
        scm,
        &format!("{}/{path}?state=opened", project(repo)?),
        1000,
    )
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "number": r["iid"],
                "user": {"login": username(&r["author"])},
                "draft": r["draft"] == true || r["work_in_progress"] == true,
                "labels": label_names(&r["labels"]).into_iter().map(|n| json!({"name": n})).collect::<Vec<_>>(),
                "head": {"ref": r["source_branch"]},
            })
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
    let kind = if is_pull_request {
        "merge_requests"
    } else {
        "issues"
    };
    scm.send(
        scm.request(
            reqwest::Method::POST,
            &format!("{}/{kind}/{number}/notes", project(repo)?),
        )
        .json(&json!({"body": body})),
        "posting GitLab note",
    )
    .await?;
    Ok(())
}

pub(super) async fn branch_sha(scm: &Scm, repo: &str, name: &str) -> Result<String> {
    branch_path(name)?;
    let v = scm
        .get(&format!(
            "{}/repository/branches/{}",
            project(repo)?,
            escape_all(name)
        ))
        .await?;
    Ok(v.pointer("/commit/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("branch response has no sha"))?
        .into())
}

pub(super) async fn create_branch(scm: &Scm, repo: &str, name: &str, sha: &str) -> Result<()> {
    branch_path(name)?;
    scm.send(
        scm.request(
            reqwest::Method::POST,
            &format!("{}/repository/branches", project(repo)?),
        )
        .query(&[("branch", name), ("ref", sha)]),
        "creating GitLab branch",
    )
    .await?;
    Ok(())
}

pub(super) async fn commit_changes(
    scm: &Scm,
    repo: &str,
    branch: &str,
    _base: &str,
    entries: &[TreeEntry],
    changes: &[FileChange],
    message: &str,
) -> Result<String> {
    let actions = Scm::checked_changes(entries, changes)?
        .into_iter()
        .map(|(ch, current)| {
            json!({
                "action": if current.is_some() { "update" } else { "create" },
                "file_path": ch.path,
                "encoding": "base64",
                "content": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, ch.content.as_bytes()),
            })
        })
        .collect::<Vec<_>>();
    let commit = scm
        .send(
            scm.request(
                reqwest::Method::POST,
                &format!("{}/repository/commits", project(repo)?),
            )
            .json(&json!({"branch": branch, "commit_message": message, "actions": actions})),
            "committing GitLab files",
        )
        .await?;
    Ok(commit
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("GitLab response has no commit id"))?
        .to_owned())
}

pub(super) async fn create_pull_request(
    scm: &Scm,
    repo: &str,
    title: &str,
    body: &str,
    head: &str,
    base: &str,
) -> Result<Value> {
    let mr = scm
        .send(
            scm.request(
                reqwest::Method::POST,
                &format!("{}/merge_requests", project(repo)?),
            )
            .json(&json!({"title": title, "description": body, "source_branch": head, "target_branch": base})),
            "creating GitLab merge request",
        )
        .await?;
    Ok(json!({"number": mr["iid"], "html_url": mr["web_url"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scm::test_server::{client, ok, serve, Reply};

    #[tokio::test]
    async fn review_and_fix_flow_uses_gitlab_endpoints() {
        let (a, c) = ("a".repeat(40), "c".repeat(40));
        let p = "/api/v4/projects/acme%2Fplatform%2Fapi";
        let (origin, server) = serve(vec![
            ok(&json!({"title":"Divide","description":"d","source_branch":"feature/x","sha":a,
                "source_project_id":1,"target_project_id":1,"target_branch":"main",
                "diff_refs":{"base_sha":"b".repeat(40)},"author":{"username":"dev"},"draft":true,
                "web_url":"https://gitlab.example/mr/7"}).to_string()),
            Reply { status: "200 OK", headers: vec![("x-next-page", String::new())],
                body: json!([{"old_path":"src/main.rs","new_path":"src/main.rs","diff":"@@ -1 +1 @@\n-a\n+b\n"}]).to_string() },
            ok("{}"),
            ok(&json!({"commit":{"id":a}}).to_string()),
            ok(&json!([{"id":"blob1","path":"src/main.rs","type":"blob","mode":"100644"}]).to_string()),
            ok(&json!({"id":c}).to_string()),
            ok(&json!({"iid":8,"web_url":"https://gitlab.example/mr/8"}).to_string()),
            ok("{}"),
        ])
        .await;
        let scm = Scm::from_connection(
            &json!({"kind":"gitlab","base_url":origin,"token":"glpat-test"}),
            &client(),
        )
        .unwrap();
        let repo = "acme/platform/api";
        let pr = scm.pull_request(repo, 7).await.unwrap();
        assert_eq!(
            (
                pr.head_ref.as_str(),
                pr.head_repo.as_str(),
                pr.author.as_str()
            ),
            ("feature/x", repo, "dev")
        );
        assert!(pr.draft);
        assert!(pr
            .diff
            .starts_with("diff --git a/src/main.rs b/src/main.rs\n"));
        scm.create_branch_from_ref(repo, "diffrook/run-1", &a)
            .await
            .unwrap();
        let sha = scm
            .commit_changes(
                repo,
                "diffrook/run-1",
                &a,
                &[FileChange {
                    path: "src/main.rs".into(),
                    content: "fn main() {}".into(),
                }],
                "fix",
            )
            .await
            .unwrap();
        assert_eq!(sha, c);
        let created = scm
            .create_pull_request(repo, "t", "b", "diffrook/run-1", "feature/x")
            .await
            .unwrap();
        assert_eq!(
            created,
            json!({"number":8,"html_url":"https://gitlab.example/mr/8"})
        );
        scm.post_comment(repo, true, 7, "looks good").await.unwrap();

        let seen = server.await.unwrap();
        let paths: Vec<_> = seen
            .iter()
            .map(|s| format!("{} {}", s.method, s.path))
            .collect();
        assert_eq!(
            paths,
            [
                format!("GET {p}/merge_requests/7"),
                format!("GET {p}/merge_requests/7/diffs?per_page=100&page=1"),
                format!("POST {p}/repository/branches?branch=diffrook%2Frun-1&ref={a}"),
                format!("GET {p}/repository/branches/diffrook%2Frun-1"),
                format!("GET {p}/repository/tree?ref={a}&recursive=true&per_page=100&page=1"),
                format!("POST {p}/repository/commits"),
                format!("POST {p}/merge_requests"),
                format!("POST {p}/merge_requests/7/notes"),
            ]
        );
        assert!(seen
            .iter()
            .all(|s| s.headers.contains("private-token: glpat-test")));
        let commit: Value = serde_json::from_str(&seen[5].body).unwrap();
        assert_eq!(commit["branch"], "diffrook/run-1");
        assert_eq!(commit["actions"][0]["action"], "update");
        assert_eq!(
            decode_utf8_base64(commit["actions"][0]["content"].as_str().unwrap()).unwrap(),
            "fn main() {}"
        );
        let mr: Value = serde_json::from_str(&seen[6].body).unwrap();
        assert_eq!(
            (mr["source_branch"].as_str(), mr["target_branch"].as_str()),
            (Some("diffrook/run-1"), Some("feature/x"))
        );
    }

    #[test]
    fn nested_groups_are_one_encoded_project_id() {
        assert_eq!(project("acme/api").unwrap(), "/projects/acme%2Fapi");
        assert_eq!(
            project("acme/platform/api").unwrap(),
            "/projects/acme%2Fplatform%2Fapi"
        );
        assert!(project("api").is_err());
        assert!(project("acme/../api").is_err());
    }
    #[test]
    fn rebuilds_unified_diff_from_gitlab_hunks() {
        let diff = unified_diff(&[
            json!({"old_path":"a.rs","new_path":"a.rs","diff":"@@ -1 +1 @@\n-x\n+y"}),
            json!({"old_path":"n.rs","new_path":"n.rs","new_file":true,"diff":"@@ -0,0 +1 @@\n+z\n"}),
        ]);
        assert_eq!(
            diff,
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/n.rs b/n.rs\n--- /dev/null\n+++ b/n.rs\n@@ -0,0 +1 @@\n+z\n"
        );
    }
}
