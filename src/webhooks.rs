//! Verify and normalize code-host webhooks. Each host signs and shapes its
//! deliveries differently; the rest of the app only sees [`Event`].
use crate::scm::{bitbucket, gitlab, Kind};
use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

/// Header names for the signature (or token), delivery id and event type.
fn header_names(kind: Kind) -> (&'static str, &'static [&'static str], &'static str) {
    match kind {
        Kind::GitHub => (
            "x-hub-signature-256",
            &["x-github-delivery"],
            "x-github-event",
        ),
        Kind::Forgejo => (
            "x-forgejo-signature",
            &["x-forgejo-delivery"],
            "x-forgejo-event",
        ),
        Kind::Gitea => ("x-gitea-signature", &["x-gitea-delivery"], "x-gitea-event"),
        // Idempotency-Key is stable across retries; older GitLab only sends the UUID.
        Kind::GitLab => (
            "x-gitlab-token",
            &["idempotency-key", "x-gitlab-event-uuid"],
            "x-gitlab-event",
        ),
        Kind::BitbucketCloud => ("x-hub-signature", &["x-request-uuid"], "x-event-key"),
        Kind::BitbucketDataCenter => ("x-hub-signature", &["x-request-id"], "x-event-key"),
    }
}

pub struct Delivery<'a> {
    pub signature: &'a str,
    pub id: &'a str,
    pub event: &'a str,
}

/// Read the delivery headers, or None when any is missing or oversized.
pub fn delivery(kind: Kind, headers: &HeaderMap) -> Option<Delivery<'_>> {
    let (signature, ids, event) = header_names(kind);
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let d = Delivery {
        signature: get(signature)?,
        id: ids.iter().find_map(|n| get(n))?,
        event: get(event)?,
    };
    (!d.signature.is_empty()
        && !d.id.is_empty()
        && !d.event.is_empty()
        && d.id.len() <= 256
        && d.event.len() <= 128)
        .then_some(d)
}

/// GitLab sends the shared secret itself; every other host signs the body
/// with HMAC-SHA256.
pub fn verify(kind: Kind, signature: &str, body: &[u8], secret: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    let expected = if kind == Kind::GitLab {
        secret.to_owned()
    } else {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        let digest = hex::encode(mac.finalize().into_bytes());
        match kind {
            Kind::Forgejo | Kind::Gitea => digest,
            _ => format!("sha256={digest}"),
        }
    };
    use subtle::ConstantTimeEq;
    signature.len() == expected.len() && bool::from(signature.as_bytes().ct_eq(expected.as_bytes()))
}

#[derive(Debug, Default, PartialEq)]
pub struct Event {
    /// One of the trigger ids automations subscribe to, such as
    /// `pull_request.opened` or `issue_comment.command`.
    pub action: &'static str,
    pub repo: String,
    /// `pull_request` or `issue`.
    pub kind: &'static str,
    pub number: Option<u64>,
    pub branch: Option<String>,
    pub actor: String,
    pub command: String,
    pub draft: bool,
    pub labels: Vec<String>,
    /// The host vouches that the commenter belongs to the repository. Only
    /// GitHub reports this; elsewhere commands need an allowed-actors list.
    pub trusted_commenter: bool,
}

/// Normalize a verified payload. None means the delivery is not an event any
/// automation can subscribe to.
pub fn parse(kind: Kind, event: &str, p: &Value) -> Option<Event> {
    let e = match kind {
        Kind::GitHub | Kind::Forgejo | Kind::Gitea => github_like(event, p),
        Kind::GitLab => gitlab_event(event, p),
        Kind::BitbucketCloud => bitbucket_cloud(event, p),
        Kind::BitbucketDataCenter => bitbucket_dc(event, p),
    }?;
    (!e.action.is_empty() && !e.repo.is_empty()).then_some(e)
}

fn text(p: &Value, pointer: &str) -> String {
    p.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn names(labels: &Value) -> Vec<String> {
    labels
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l["name"].as_str().or(l.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn github_like(event: &str, p: &Value) -> Option<Event> {
    let base_event = match event {
        "pull_request.synchronized" => "pull_request",
        other => other,
    };
    let action = match (base_event, p["action"].as_str().unwrap_or("")) {
        ("pull_request", "opened") => "pull_request.opened",
        ("pull_request", "synchronize" | "synchronized") => "pull_request.synchronize",
        ("pull_request", "ready_for_review") => "pull_request.ready_for_review",
        ("issues", "opened") => "issues.opened",
        ("issues", "labeled") => "issues.labeled",
        ("issue_comment", "created") => "issue_comment.command",
        _ => "",
    };
    let is_pr = event.starts_with("pull_request") || p["issue"]["pull_request"].is_object();
    let association = p["comment"]["author_association"]
        .as_str()
        .or(p["sender"]["author_association"].as_str())
        .unwrap_or("");
    Some(Event {
        action,
        repo: p["repository"]["full_name"]
            .as_str()
            .or(p["repository"]["name"].as_str())?
            .to_owned(),
        kind: if is_pr { "pull_request" } else { "issue" },
        number: if is_pr {
            p["pull_request"]["number"]
                .as_u64()
                .or(p["issue"]["number"].as_u64())
        } else {
            p["issue"]["number"].as_u64()
        },
        branch: is_pr
            .then(|| p["pull_request"]["head"]["ref"].as_str().map(str::to_owned))
            .flatten(),
        actor: p["sender"]["login"]
            .as_str()
            .or(p["sender"]["username"].as_str())
            .unwrap_or("")
            .to_owned(),
        command: text(p, "/comment/body"),
        draft: p["pull_request"]["draft"] == true,
        labels: names(
            p["pull_request"]["labels"]
                .as_array()
                .map(|_| &p["pull_request"]["labels"])
                .unwrap_or(&p["issue"]["labels"]),
        ),
        trusted_commenter: matches!(association, "OWNER" | "MEMBER" | "COLLABORATOR"),
    })
}

fn gitlab_event(event: &str, p: &Value) -> Option<Event> {
    let attrs = &p["object_attributes"];
    let mut e = Event {
        repo: text(p, "/project/path_with_namespace"),
        actor: text(p, "/user/username"),
        labels: gitlab::label_names(&p["labels"]),
        ..Event::default()
    };
    match event {
        "Merge Request Hook" => {
            let draft_cleared = p.pointer("/changes/draft/previous") == Some(&Value::Bool(true))
                && p.pointer("/changes/draft/current") == Some(&Value::Bool(false));
            e.action = match attrs["action"].as_str().unwrap_or("") {
                "open" => "pull_request.opened",
                // oldrev is only present when new commits were pushed.
                "update" if attrs["oldrev"].is_string() => "pull_request.synchronize",
                "update" if draft_cleared => "pull_request.ready_for_review",
                _ => "",
            };
            e.kind = "pull_request";
            e.number = attrs["iid"].as_u64();
            e.branch = attrs["source_branch"].as_str().map(str::to_owned);
            e.draft = attrs["draft"] == true || attrs["work_in_progress"] == true;
        }
        "Issue Hook" | "Confidential Issue Hook" => {
            let previous = gitlab::label_names(&p["changes"]["labels"]["previous"]);
            let added = gitlab::label_names(&p["changes"]["labels"]["current"])
                .iter()
                .any(|l| !previous.contains(l));
            e.action = match attrs["action"].as_str().unwrap_or("") {
                "open" => "issues.opened",
                "update" if added => "issues.labeled",
                _ => "",
            };
            e.kind = "issue";
            e.number = attrs["iid"].as_u64();
        }
        "Note Hook" | "Confidential Note Hook" => {
            if attrs["action"].as_str().is_some_and(|a| a != "create") {
                return None;
            }
            e.action = "issue_comment.command";
            e.command = text(attrs, "/note");
            match attrs["noteable_type"].as_str()? {
                "MergeRequest" => {
                    let mr = &p["merge_request"];
                    e.kind = "pull_request";
                    e.number = mr["iid"].as_u64();
                    e.branch = mr["source_branch"].as_str().map(str::to_owned);
                    e.draft = mr["draft"] == true || mr["work_in_progress"] == true;
                    e.labels = gitlab::label_names(&mr["labels"]);
                }
                "Issue" => {
                    e.kind = "issue";
                    e.number = p["issue"]["iid"].as_u64();
                    e.labels = gitlab::label_names(&p["issue"]["labels"]);
                }
                _ => return None,
            }
        }
        _ => return None,
    }
    Some(e)
}

fn bitbucket_user(v: &Value) -> String {
    ["nickname", "name", "display_name"]
        .iter()
        .find_map(|k| v[*k].as_str())
        .unwrap_or_default()
        .to_owned()
}

fn bitbucket_cloud(event: &str, p: &Value) -> Option<Event> {
    let mut e = Event {
        repo: text(p, "/repository/full_name"),
        actor: bitbucket_user(&p["actor"]),
        command: text(p, "/comment/content/raw"),
        ..Event::default()
    };
    if let Some(pr) = p.get("pullrequest") {
        e.action = match event {
            "pullrequest:created" => "pull_request.opened",
            // Bitbucket has no push-only event; updated also fires on edits.
            "pullrequest:updated" => "pull_request.synchronize",
            "pullrequest:comment_created" => "issue_comment.command",
            _ => "",
        };
        e.kind = "pull_request";
        e.number = pr["id"].as_u64();
        e.branch = pr
            .pointer("/source/branch/name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        e.draft = pr["draft"] == true;
    } else {
        let issue = p.get("issue")?;
        let classified = ["kind", "component"]
            .iter()
            .any(|k| p["changes"].get(*k).is_some());
        e.action = match event {
            "issue:created" => "issues.opened",
            "issue:updated" if classified => "issues.labeled",
            "issue:comment_created" => "issue_comment.command",
            _ => "",
        };
        e.kind = "issue";
        e.number = issue["id"].as_u64();
        e.labels = bitbucket::issue_labels(issue);
    }
    Some(e)
}

fn bitbucket_dc(event: &str, p: &Value) -> Option<Event> {
    let pr = p.get("pullRequest")?;
    Some(Event {
        action: match event {
            "pr:opened" => "pull_request.opened",
            "pr:from_ref_updated" => "pull_request.synchronize",
            "pr:comment:added" => "issue_comment.command",
            _ => "",
        },
        repo: format!(
            "{}/{}",
            text(pr, "/toRef/repository/project/key"),
            text(pr, "/toRef/repository/slug")
        ),
        kind: "pull_request",
        number: pr["id"].as_u64(),
        branch: pr
            .pointer("/fromRef/displayId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        actor: bitbucket_user(&p["actor"]),
        command: text(p, "/comment/text"),
        draft: pr["draft"] == true,
        labels: Vec::new(),
        trusted_commenter: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn signatures_follow_each_host_scheme() {
        let body = br#"{"a":1}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
        mac.update(body);
        let hex = hex::encode(mac.finalize().into_bytes());
        assert!(verify(
            Kind::GitHub,
            &format!("sha256={hex}"),
            body,
            "s3cret"
        ));
        assert!(verify(
            Kind::BitbucketCloud,
            &format!("sha256={hex}"),
            body,
            "s3cret"
        ));
        assert!(verify(
            Kind::BitbucketDataCenter,
            &format!("sha256={hex}"),
            body,
            "s3cret"
        ));
        assert!(verify(Kind::Gitea, &hex, body, "s3cret"));
        assert!(!verify(
            Kind::Gitea,
            &format!("sha256={hex}"),
            body,
            "s3cret"
        ));
        assert!(verify(Kind::GitLab, "s3cret", body, "s3cret"));
        assert!(!verify(Kind::GitLab, "wrong!", body, "s3cret"));
        assert!(!verify(Kind::GitLab, "", body, ""));
    }

    #[test]
    fn gitlab_delivery_prefers_the_retry_stable_key() {
        let mut h = HeaderMap::new();
        h.insert("x-gitlab-token", "t".parse().unwrap());
        h.insert("x-gitlab-event", "Merge Request Hook".parse().unwrap());
        h.insert("x-gitlab-event-uuid", "uuid".parse().unwrap());
        assert_eq!(delivery(Kind::GitLab, &h).unwrap().id, "uuid");
        h.insert("idempotency-key", "stable".parse().unwrap());
        assert_eq!(delivery(Kind::GitLab, &h).unwrap().id, "stable");
        h.remove("x-gitlab-token");
        assert!(delivery(Kind::GitLab, &h).is_none());
    }

    #[test]
    fn gitlab_merge_requests_issues_and_notes() {
        let opened = json!({"project":{"path_with_namespace":"acme/platform/api"},"user":{"username":"dev"},
            "labels":[{"title":"backend"}],
            "object_attributes":{"action":"open","iid":7,"source_branch":"feature/x","draft":true}});
        let e = parse(Kind::GitLab, "Merge Request Hook", &opened).unwrap();
        assert_eq!(
            e,
            Event {
                action: "pull_request.opened",
                repo: "acme/platform/api".into(),
                kind: "pull_request",
                number: Some(7),
                branch: Some("feature/x".into()),
                actor: "dev".into(),
                draft: true,
                labels: vec!["backend".into()],
                ..Event::default()
            }
        );
        let push = json!({"project":{"path_with_namespace":"acme/api"},"object_attributes":{"action":"update","iid":7,"oldrev":"abc"}});
        assert_eq!(
            parse(Kind::GitLab, "Merge Request Hook", &push)
                .unwrap()
                .action,
            "pull_request.synchronize"
        );
        let edit = json!({"project":{"path_with_namespace":"acme/api"},"object_attributes":{"action":"update","iid":7}});
        assert!(parse(Kind::GitLab, "Merge Request Hook", &edit).is_none());
        let labeled = json!({"project":{"path_with_namespace":"acme/api"},"object_attributes":{"action":"update","iid":3},
            "changes":{"labels":{"previous":[],"current":[{"title":"autofix"}]}}});
        assert_eq!(
            parse(Kind::GitLab, "Issue Hook", &labeled).unwrap().action,
            "issues.labeled"
        );
        let note = json!({"project":{"path_with_namespace":"acme/api"},"user":{"username":"dev"},
            "object_attributes":{"note":"/diffrook review","noteable_type":"MergeRequest"},
            "merge_request":{"iid":9,"source_branch":"fix"}});
        let e = parse(Kind::GitLab, "Note Hook", &note).unwrap();
        assert_eq!(
            (e.action, e.kind, e.number, e.command.as_str()),
            (
                "issue_comment.command",
                "pull_request",
                Some(9),
                "/diffrook review"
            )
        );
        assert!(!e.trusted_commenter);
    }

    #[test]
    fn bitbucket_cloud_and_data_center_pull_requests() {
        let cloud = json!({"repository":{"full_name":"acme/api"},"actor":{"nickname":"dev"},
            "pullrequest":{"id":4,"draft":false,"source":{"branch":{"name":"feature"}}}});
        let e = parse(Kind::BitbucketCloud, "pullrequest:created", &cloud).unwrap();
        assert_eq!(
            (
                e.action,
                e.repo.as_str(),
                e.number,
                e.branch.as_deref(),
                e.actor.as_str()
            ),
            (
                "pull_request.opened",
                "acme/api",
                Some(4),
                Some("feature"),
                "dev"
            )
        );
        let issue = json!({"repository":{"full_name":"acme/api"},"actor":{"nickname":"dev"},
            "issue":{"id":2,"kind":"bug","component":{"name":"autofix"}},"changes":{"component":{"old":null,"new":"autofix"}}});
        let e = parse(Kind::BitbucketCloud, "issue:updated", &issue).unwrap();
        assert_eq!(
            (e.action, e.kind, e.labels.clone()),
            (
                "issues.labeled",
                "issue",
                vec!["bug".to_string(), "autofix".to_string()]
            )
        );

        let dc = json!({"actor":{"name":"dev","slug":"dev"},"comment":{"text":"/diffrook"},
            "pullRequest":{"id":12,"fromRef":{"displayId":"feature"},
                "toRef":{"repository":{"slug":"api","project":{"key":"ACME"}}}}});
        let e = parse(Kind::BitbucketDataCenter, "pr:comment:added", &dc).unwrap();
        assert_eq!(
            (e.action, e.repo.as_str(), e.number, e.command.as_str()),
            ("issue_comment.command", "ACME/api", Some(12), "/diffrook")
        );
        assert!(parse(Kind::BitbucketDataCenter, "repo:refs_changed", &json!({})).is_none());
    }

    #[test]
    fn github_comment_trust_comes_from_author_association() {
        let p = json!({"action":"created","repository":{"full_name":"acme/api"},"sender":{"login":"dev"},
            "issue":{"number":5,"pull_request":{}},"comment":{"body":"/diffrook","author_association":"MEMBER"}});
        let e = parse(Kind::GitHub, "issue_comment", &p).unwrap();
        assert_eq!((e.kind, e.number), ("pull_request", Some(5)));
        assert!(e.trusted_commenter);
    }
}
