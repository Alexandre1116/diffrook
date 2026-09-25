use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub full_name: String,
    pub default_branch: String,
    pub html_url: String,
    pub fork: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub sha: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub sha: String,
    pub size: Option<u64>,
    pub kind: String,
    pub mode: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub head_ref: String,
    pub head_sha: String,
    pub head_repo: String,
    pub base_ref: String,
    pub base_sha: String,
    pub diff: String,
    pub author: String,
    pub draft: bool,
    pub html_url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: String,
    pub labels: Vec<String>,
    pub comments: Vec<String>,
    pub html_url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub sha: String,
    pub size: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub content: String,
}

#[derive(Clone)]
pub struct Scm {
    kind: String,
    base: String,
    token: String,
    client: reqwest::Client,
}

impl Scm {
    pub fn from_connection(c: &Value, client: &reqwest::Client) -> Result<Self> {
        let kind = required(c, "kind")?.to_owned();
        if kind != "github" && kind != "forgejo" {
            bail!("unsupported connection kind: {kind}");
        }
        let base = c
            .get("base_url")
            .and_then(Value::as_str)
            .filter(|x| !x.is_empty())
            .unwrap_or(if kind == "github" {
                "https://api.github.com"
            } else {
                ""
            });
        if base.is_empty() {
            bail!("Forgejo connection is missing base_url");
        }
        let token = required(c, "token")?.to_owned();
        let base = if kind == "forgejo" {
            format!("{}/api/v1", base.trim_end_matches('/'))
        } else {
            base.trim_end_matches('/').to_owned()
        };
        Ok(Self {
            kind,
            base,
            token,
            client: client.clone(),
        })
    }
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .request(method, self.url(path))
            .bearer_auth(&self.token)
            .header(
                reqwest::header::ACCEPT,
                if self.kind == "github" {
                    "application/vnd.github+json"
                } else {
                    "application/json"
                },
            );
        if self.kind == "github" {
            request = request
                .header("x-github-api-version", "2022-11-28")
                .header("user-agent", "Diffrook");
        }
        request
    }
    async fn get(&self, path: &str) -> Result<Value> {
        self.json(
            self.request(reqwest::Method::GET, path)
                .send()
                .await
                .context("SCM GET failed")?,
        )
        .await
    }
    async fn json(&self, response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let body = crate::network::read_body(response, 32 * 1024 * 1024).await?;
        if !status.is_success() {
            bail!("SCM returned HTTP {status}");
        }
        serde_json::from_slice(&body).context("SCM returned invalid JSON")
    }
    pub async fn repository(&self, repo: &str) -> Result<Repository> {
        let value = self.get(&format!("/repos/{}", repo_path(repo)?)).await?;
        Ok(Repository {
            full_name: value
                .get("full_name")
                .and_then(Value::as_str)
                .unwrap_or(repo)
                .to_owned(),
            default_branch: value
                .get("default_branch")
                .and_then(Value::as_str)
                .unwrap_or("main")
                .to_owned(),
            html_url: value
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            fork: value.get("fork").and_then(Value::as_bool).unwrap_or(false),
        })
    }
    pub async fn tree(&self, repo: &str, branch: &str) -> Result<Vec<TreeEntry>> {
        let branch_data = self
            .get(&format!(
                "/repos/{}/branches/{}",
                repo_path(repo)?,
                branch_path(branch)?
            ))
            .await?;
        let sha = branch_data
            .pointer("/commit/sha")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("branch response has no commit sha"))?;
        self.tree_at(repo, sha).await
    }
    pub async fn tree_at(&self, repo: &str, sha: &str) -> Result<Vec<TreeEntry>> {
        let mut entries = Vec::new();
        if self.kind == "forgejo" {
            for page in 1..=1000 {
                let data = self
                    .get(&format!(
                        "/repos/{}/git/trees/{}?recursive=true&page={page}&per_page=100",
                        repo_path(repo)?,
                        segment(sha)?
                    ))
                    .await?;
                let rows = data
                    .get("tree")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("tree response has no tree array"))?;
                let more = data
                    .get("truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(rows.len() >= 100);
                append_tree_rows(&mut entries, rows)?;
                if !more {
                    break;
                }
                if page == 1000 {
                    bail!("Forgejo tree pagination exceeded 1000 pages")
                }
            }
        } else {
            let data = self
                .get(&format!(
                    "/repos/{}/git/trees/{}?recursive=1",
                    repo_path(repo)?,
                    segment(sha)?
                ))
                .await?;
            let rows = data
                .get("tree")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("tree response has no tree array"))?;
            append_tree_rows(&mut entries, rows)?;
            if data
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                bail!("GitHub tree exceeds API limits; refusing incomplete coverage");
            }
        }
        Ok(entries)
    }
    pub async fn file(&self, repo: &str, path: &str, reference: &str) -> Result<FileContent> {
        validate_path(path)?;
        let reference =
            url::form_urlencoded::byte_serialize(reference.as_bytes()).collect::<String>();
        let data = self
            .get(&format!(
                "/repos/{}/contents/{}?ref={}",
                repo_path(repo)?,
                path.split('/')
                    .map(segment)
                    .collect::<Result<Vec<_>>>()?
                    .join("/"),
                reference
            ))
            .await?;
        let encoded = data
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("SCM file response has no content"))?;
        let decoded = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            encoded.replace('\n', ""),
        )
        .context("decoding repository file")?;
        let content =
            String::from_utf8(decoded).map_err(|_| anyhow!("repository file is not UTF-8"))?;
        Ok(FileContent {
            path: path.into(),
            size: content.len(),
            content,
            sha: data
                .get("sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
        })
    }
    pub async fn pull_request(&self, repo: &str, number: u64) -> Result<PullRequest> {
        let data = self
            .get(&format!("/repos/{}/pulls/{number}", repo_path(repo)?))
            .await?;
        let diff_request = if self.kind == "forgejo" {
            self.request(
                reqwest::Method::GET,
                &format!("/repos/{}/pulls/{number}.diff", repo_path(repo)?),
            )
        } else {
            self.request(
                reqwest::Method::GET,
                &format!("/repos/{}/pulls/{number}", repo_path(repo)?),
            )
            .header(reqwest::header::ACCEPT, "application/vnd.github.v3.diff")
        };
        let diff_response = diff_request
            .send()
            .await
            .context("fetching pull request diff")?;
        let status = diff_response.status();
        if !status.is_success() {
            bail!("SCM returned HTTP {status} for pull request diff");
        }
        let diff =
            String::from_utf8(crate::network::read_body(diff_response, 16 * 1024 * 1024).await?)
                .context("pull request diff is not UTF-8")?;
        Ok(PullRequest {
            number,
            title: str_at(&data, "title"),
            body: str_at(&data, "body"),
            head_ref: data
                .pointer("/head/ref")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            head_sha: data
                .pointer("/head/sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            head_repo: data
                .pointer("/head/repo/full_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            base_ref: data
                .pointer("/base/ref")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            base_sha: data
                .pointer("/base/sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            author: data
                .pointer("/user/login")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            draft: data.get("draft").and_then(Value::as_bool).unwrap_or(false),
            html_url: str_at(&data, "html_url"),
            diff,
        })
    }
    pub async fn issue(&self, repo: &str, number: u64) -> Result<Issue> {
        let data = self
            .get(&format!("/repos/{}/issues/{number}", repo_path(repo)?))
            .await?;
        let comments_data = self
            .get(&format!(
                "/repos/{}/issues/{number}/comments?per_page=100",
                repo_path(repo)?
            ))
            .await?;
        let comments = comments_data
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        format!(
                            "{}: {}",
                            x.pointer("/user/login")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown"),
                            x.get("body").and_then(Value::as_str).unwrap_or_default()
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let labels = data
            .get("labels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Issue {
            number,
            title: str_at(&data, "title"),
            body: str_at(&data, "body"),
            author: data
                .pointer("/user/login")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            labels,
            comments,
            html_url: str_at(&data, "html_url"),
        })
    }
    pub async fn open_pull_requests(&self, repo: &str) -> Result<Vec<Value>> {
        self.list_open(repo, "pulls", false).await
    }
    pub async fn open_issues(&self, repo: &str) -> Result<Vec<Value>> {
        self.list_open(repo, "issues", true).await
    }
    async fn list_open(&self, repo: &str, kind: &str, filter_prs: bool) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        for page in 1..=1000 {
            let endpoint = if self.kind == "github" {
                format!(
                    "/repos/{}/{}?state=open&per_page=100&page={page}",
                    repo_path(repo)?,
                    kind
                )
            } else {
                format!(
                    "/repos/{}/{}?state=open&limit=50&page={page}",
                    repo_path(repo)?,
                    kind
                )
            };
            let rows = self.get(&endpoint).await?;
            let rows = rows
                .as_array()
                .ok_or_else(|| anyhow!("SCM list response is not an array"))?;
            let page_len = rows.len();
            for row in rows {
                if filter_prs && row.get("pull_request").is_some() {
                    continue;
                }
                result.push(row.clone());
            }
            let page_size = if self.kind == "github" { 100 } else { 50 };
            if page_len < page_size {
                break;
            }
            if page == 1000 {
                bail!("SCM pagination exceeded 1000 pages")
            }
        }
        Ok(result)
    }
    pub async fn post_comment(&self, repo: &str, number: u64, body: &str) -> Result<()> {
        self.json(
            self.request(
                reqwest::Method::POST,
                &format!("/repos/{}/issues/{number}/comments", repo_path(repo)?),
            )
            .json(&json!({"body":body}))
            .send()
            .await
            .context("posting SCM comment")?,
        )
        .await?;
        Ok(())
    }
    pub async fn create_branch(&self, repo: &str, name: &str, from_sha: &str) -> Result<()> {
        if self.kind == "forgejo" {
            return self
                .json(
                    self.request(
                        reqwest::Method::POST,
                        &format!("/repos/{}/branches", repo_path(repo)?),
                    )
                    .json(&json!({"new_branch_name":name,"old_ref_name":from_sha}))
                    .send()
                    .await
                    .context("creating Forgejo branch")?,
                )
                .await
                .map(|_| ());
        }
        self.create_github_branch(repo, name, from_sha).await
    }
    async fn create_github_branch(&self, repo: &str, name: &str, sha: &str) -> Result<()> {
        self.json(
            self.request(
                reqwest::Method::POST,
                &format!("/repos/{}/git/refs", repo_path(repo)?),
            )
            .json(&json!({"ref":format!("refs/heads/{name}"),"sha":sha}))
            .send()
            .await
            .context("creating SCM branch")?,
        )
        .await?;
        Ok(())
    }
    pub async fn create_branch_from_ref(
        &self,
        repo: &str,
        name: &str,
        old_ref: &str,
    ) -> Result<()> {
        if self.kind == "forgejo" {
            self.json(
                self.request(
                    reqwest::Method::POST,
                    &format!("/repos/{}/branches", repo_path(repo)?),
                )
                .json(&json!({"new_branch_name":name,"old_ref_name":old_ref}))
                .send()
                .await
                .context("creating Forgejo branch")?,
            )
            .await?;
            Ok(())
        } else {
            let sha = if old_ref.len() == 40 && old_ref.bytes().all(|b| b.is_ascii_hexdigit()) {
                old_ref.to_owned()
            } else {
                self.branch_sha(repo, old_ref).await?
            };
            self.create_github_branch(repo, name, &sha).await
        }
    }
    pub async fn branch_sha(&self, repo: &str, name: &str) -> Result<String> {
        let v = self
            .get(&format!(
                "/repos/{}/branches/{}",
                repo_path(repo)?,
                branch_path(name)?
            ))
            .await?;
        Ok(v.pointer("/commit/sha")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("branch response has no sha"))?
            .into())
    }
    pub async fn commit_changes(
        &self,
        repo: &str,
        branch: &str,
        expected_sha: &str,
        changes: &[FileChange],
        message: &str,
    ) -> Result<String> {
        if changes.is_empty() {
            bail!("cannot publish an empty change set")
        }
        let base = self.branch_sha(repo, branch).await?;
        if base != expected_sha {
            bail!("branch changed before commit; refusing stale changes")
        }
        let entries = self.tree_at(repo, &base).await?;
        if self.kind == "forgejo" {
            let mut seen = std::collections::HashSet::new();
            let mut files = Vec::new();
            for ch in changes {
                validate_path(&ch.path)?;
                if !seen.insert(ch.path.clone()) {
                    bail!("duplicate changed path {}", ch.path)
                }
                let current = entries.iter().find(|entry| entry.path == ch.path);
                let mut file = json!({"path":ch.path,"operation":if current.is_some(){"update"}else{"create"},"content":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,ch.content.as_bytes())});
                if let Some(old) = current {
                    if !["100644", "100755"].contains(&old.mode.as_str()) {
                        bail!("cannot modify symlink or submodule {}", ch.path)
                    }
                    file["sha"] = json!(old.sha);
                }
                files.push(file);
            }
            let result = self
                .json(
                    self.request(
                        reqwest::Method::POST,
                        &format!("/repos/{}/contents", repo_path(repo)?),
                    )
                    .json(&json!({"branch":branch,"message":message,"files":files}))
                    .send()
                    .await
                    .context("committing Forgejo files")?,
                )
                .await?;
            return Ok(result
                .pointer("/commit/sha")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Forgejo response has no commit sha"))?
                .to_owned());
        }
        let base_tree = self
            .get(&format!(
                "/repos/{}/git/commits/{}",
                repo_path(repo)?,
                segment(&base)?
            ))
            .await?
            .pointer("/tree/sha")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("base commit response has no tree sha"))?
            .to_owned();
        let mut tree = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for ch in changes {
            validate_path(&ch.path)?;
            if !seen.insert(ch.path.clone()) {
                bail!("duplicate changed path {}", ch.path)
            }
            let mode = entries
                .iter()
                .find(|e| e.path == ch.path)
                .map(|e| e.mode.as_str())
                .unwrap_or("100644");
            if !["100644", "100755"].contains(&mode) {
                bail!("cannot modify symlink or submodule {}", ch.path)
            }
            tree.push(json!({"path":ch.path,"mode":mode,"type":"blob","content":ch.content}));
        }
        let tree_data = self
            .json(
                self.request(
                    reqwest::Method::POST,
                    &format!("/repos/{}/git/trees", repo_path(repo)?),
                )
                .json(&json!({"base_tree":base_tree,"tree":tree}))
                .send()
                .await
                .context("creating SCM tree")?,
            )
            .await?;
        let tree_sha = tree_data
            .get("sha")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("created tree has no sha"))?;
        let commit = self
            .json(
                self.request(
                    reqwest::Method::POST,
                    &format!("/repos/{}/git/commits", repo_path(repo)?),
                )
                .json(&json!({"message":message,"tree":tree_sha,"parents":[base]}))
                .send()
                .await
                .context("creating SCM commit")?,
            )
            .await?;
        let commit_sha = commit
            .get("sha")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("created commit has no sha"))?;
        self.json(
            self.request(
                reqwest::Method::PATCH,
                &format!(
                    "/repos/{}/git/refs/heads/{}",
                    repo_path(repo)?,
                    branch_path(branch)?
                ),
            )
            .json(&json!({"sha":commit_sha,"force":false}))
            .send()
            .await
            .context("updating SCM branch")?,
        )
        .await?;
        Ok(commit_sha.into())
    }
    pub async fn create_pull_request(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<Value> {
        self.json(
            self.request(
                reqwest::Method::POST,
                &format!("/repos/{}/pulls", repo_path(repo)?),
            )
            .json(&json!({"title":title,"body":body,"head":head,"base":base}))
            .send()
            .await
            .context("creating pull request")?,
        )
        .await
    }
}

pub async fn test_connection(connection: &Value, client: &reqwest::Client) -> Result<String> {
    let scm = Scm::from_connection(connection, client)?;
    let response = scm
        .request(reqwest::Method::GET, "/user")
        .send()
        .await
        .context("testing SCM connection")?;
    let status = response.status();
    if !status.is_success() {
        bail!("SCM returned HTTP {status} for /user")
    }
    let data: Value = response.json().await.context("decoding SCM identity")?;
    Ok(format!(
        "Connected as {}",
        data.get("login")
            .or_else(|| data.get("username"))
            .and_then(Value::as_str)
            .unwrap_or("user")
    ))
}

pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path.contains('\0')
    {
        bail!("unsafe repository path");
    }
    if path
        .split('/')
        .any(|p| p.is_empty() || p == "." || p == "..")
    {
        bail!("unsafe repository path");
    }
    Ok(())
}
fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("connection is missing {k}"))
}
fn repo_path(repo: &str) -> Result<String> {
    let parts: Vec<_> = repo.split('/').collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|p| p.is_empty() || p == &"." || p == &".." || p.contains('?'))
    {
        bail!("repository must be owner/name")
    };
    Ok(parts
        .iter()
        .map(|p| segment(p))
        .collect::<Result<Vec<_>>>()?
        .join("/"))
}
fn segment(s: &str) -> Result<String> {
    if s.is_empty() || s == "." || s == ".." || s.contains('/') || s.contains('\\') {
        bail!("invalid URL path segment")
    };
    Ok(url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20"))
}
fn branch_path(s: &str) -> Result<String> {
    if s.is_empty()
        || s.starts_with('/')
        || s.ends_with('/')
        || s.split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.contains('\\'))
    {
        bail!("invalid branch name")
    };
    Ok(s.split('/')
        .map(segment)
        .collect::<Result<Vec<_>>>()?
        .join("/"))
}
fn append_tree_rows(entries: &mut Vec<TreeEntry>, rows: &[Value]) -> Result<()> {
    for row in rows {
        let path = row
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if path.is_empty() {
            continue;
        }
        validate_path(&path)?;
        entries.push(TreeEntry {
            path,
            sha: row
                .get("sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            size: row.get("size").and_then(Value::as_u64),
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
    Ok(())
}
fn str_at(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_repository_paths() {
        assert!(validate_path("src/main.rs").is_ok());
        for bad in ["../secret", "/etc/passwd", "a\\b", "a//b"] {
            assert!(validate_path(bad).is_err(), "{bad}")
        }
    }
    #[test]
    fn repository_names_are_exactly_owner_slash_name() {
        assert!(repo_path("owner/repo").is_ok());
        assert!(repo_path("owner/repo/extra").is_err());
        assert!(repo_path("../repo").is_err())
    }
    #[test]
    fn branch_names_may_have_safe_slash_components() {
        assert_eq!(branch_path("diffrook/run-1").unwrap(), "diffrook/run-1");
        assert!(branch_path("../main").is_err())
    }
    #[tokio::test]
    async fn forgejo_uses_native_branch_and_contents_endpoints() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let expected = [
                (
                    "POST",
                    "/api/v1/repos/acme/widget/branches",
                    r#"{"new_branch_name":"diffrook/run-1","old_ref_name":"main"}"#,
                ),
                (
                    "GET",
                    "/api/v1/repos/acme/widget/branches/diffrook/run-1",
                    "",
                ),
                (
                    "GET",
                    "/api/v1/repos/acme/widget/git/trees/aabbcc?recursive=true&page=1&per_page=100",
                    r#""#,
                ),
                ("POST", "/api/v1/repos/acme/widget/contents", r#""#),
            ];
            let mut seen = Vec::new();
            for (index, (method, path, body_expected)) in expected.iter().enumerate() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length: ")
                                    .and_then(|n| n.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                let head = String::from_utf8_lossy(&bytes[..end]);
                let first = head.lines().next().unwrap();
                let mut parts = first.split_whitespace();
                let got_method = parts.next().unwrap();
                let got_path = parts.next().unwrap();
                let body = String::from_utf8_lossy(&bytes[end + 4..]);
                seen.push((got_method.to_owned(), got_path.to_owned(), body.to_string()));
                assert_eq!(got_method, *method);
                assert_eq!(got_path, *path);
                if !body_expected.is_empty() {
                    let expected: Value = serde_json::from_str(body_expected).unwrap();
                    let actual: Value = serde_json::from_str(&body).unwrap();
                    for (k, v) in expected.as_object().unwrap() {
                        assert_eq!(&actual[k], v)
                    }
                }
                let response = match index {
                    0 => r#"{"name":"diffrook/run-1"}"#,
                    1 => r#"{"name":"diffrook/run-1","commit":{"sha":"aabbcc"}}"#,
                    2 => {
                        r#"{"truncated":false,"tree":[{"path":"src/main.rs","type":"blob","mode":"100644","sha":"old-file"}]}"#
                    }
                    _ => r#"{"commit":{"sha":"new-commit"}}"#,
                };
                let status = if index == 0 || index == 3 {
                    "201 Created"
                } else {
                    "200 OK"
                };
                let wire=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len());
                stream.write_all(wire.as_bytes()).await.unwrap();
            }
            seen
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let conn =
            json!({"kind":"forgejo","base_url":format!("http://{address}"),"token":"test-token"});
        let scm = Scm::from_connection(&conn, &client).unwrap();
        scm.create_branch_from_ref("acme/widget", "diffrook/run-1", "main")
            .await
            .unwrap();
        let sha = scm
            .commit_changes(
                "acme/widget",
                "diffrook/run-1",
                "aabbcc",
                &[FileChange {
                    path: "src/main.rs".into(),
                    content: "fn main() {}".into(),
                }],
                "fix",
            )
            .await
            .unwrap();
        assert_eq!(sha, "new-commit");
        let seen = server.await.unwrap();
        assert!(seen[0].2.contains("old_ref_name"));
        let upload: Value = serde_json::from_str(&seen[3].2).unwrap();
        assert_eq!(upload["branch"], "diffrook/run-1");
        assert_eq!(
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                upload["files"][0]["content"].as_str().unwrap()
            )
            .unwrap(),
            b"fn main() {}"
        );
        assert_eq!(upload["files"][0]["sha"], "old-file");
        assert_eq!(upload["files"][0]["operation"], "update");
    }
}
