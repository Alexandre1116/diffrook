use crate::{core::AppState, db, routes};
use serde_json::json;
use std::str::FromStr;

pub fn start(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            if let Err(e) = process_due(&state).await {
                tracing::error!("scheduler error: {e}");
            }
        }
    });
}

pub async fn process_due(state: &AppState) -> anyhow::Result<usize> {
    let automations = db::objects(&state.db, "automations").await?;
    let now = chrono::Utc::now();
    let mut count = 0;
    for a in automations {
        if a["enabled"] != true || a["trigger"]["schedule_enabled"] != true {
            continue;
        }
        let cron = a["trigger"]["cron"].as_str().unwrap_or("");
        let normalized = match routes::normalize_cron(cron) {
            Ok(x) => x,
            Err(_) => {
                tracing::warn!("skipping automation {} with invalid cron", a["id"]);
                continue;
            }
        };
        let schedule = match cron::Schedule::from_str(&normalized) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("skipping automation {} with invalid cron: {e}", a["id"]);
                continue;
            }
        };
        let tz = match a["trigger"]["timezone"]
            .as_str()
            .unwrap_or("UTC")
            .parse::<chrono_tz::Tz>()
        {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("skipping automation {} with invalid timezone: {e}", a["id"]);
                continue;
            }
        };
        let local_now = now.with_timezone(&tz);
        let floor = local_now - chrono::Duration::seconds(70);
        let due = schedule.after(&floor).next();
        let Some(slot) = due else { continue };
        if slot > local_now {
            continue;
        }
        let mark = slot.format("%Y-%m-%dT%H:%M:%S%:z").to_string();
        let id = a["id"].as_str().unwrap_or("");
        let target = a["trigger"]["schedule_target"]
            .as_str()
            .unwrap_or("repository");
        let repositories = a["repositories"].as_array().cloned().unwrap_or_default();
        for repository in repositories {
            let repo = repository.as_str().unwrap_or("");
            if repo.is_empty() {
                continue;
            }
            let prev = sqlx::query_scalar::<_, String>(
                "SELECT last_slot FROM scheduler_marks_v2 WHERE automation_id=? AND repository=?",
            )
            .bind(id)
            .bind(repo)
            .fetch_optional(&state.db)
            .await?;
            if prev.as_deref() == Some(mark.as_str()) {
                continue;
            }
            let base = json!({"repository":repo,"scheduled_at":slot.with_timezone(&chrono::Utc).to_rfc3339()});
            if target == "repository" {
                let mut trigger = base.clone();
                trigger["kind"] = json!("repository");
                trigger["branch"] = a["trigger"]["branch"].clone();
                if routes::run_queue(state.clone(), id.to_owned(), trigger)
                    .await
                    .is_ok()
                {
                    count += 1;
                    save_mark(state, id, repo, &mark).await?
                }
                continue;
            }
            let mut connection = match db::object(
                &state.db,
                "connections",
                a["connection_id"].as_str().unwrap_or(""),
            )
            .await?
            {
                Some(c) => c,
                None => continue,
            };
            if state.reveal("connections", &mut connection).is_err() {
                continue;
            }
            let scm = match crate::scm::Scm::from_connection(&connection, &state.client) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let (kind, items) = if target == "open_pull_requests" {
                ("pull_request", scm.open_pull_requests(repo).await)
            } else if target == "open_issues" {
                ("issue", scm.open_issues(repo).await)
            } else {
                continue;
            };
            let items = match items {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("schedule enumeration failed for {repo}: {e}");
                    continue;
                }
            };
            let mut queue_failed = false;
            for item in items {
                if let Some(allowed) = a["filters"]["allowed_actors"].as_array() {
                    if !allowed.is_empty()
                        && !allowed.iter().any(|actor| {
                            actor.as_str().is_some_and(|actor| {
                                actor.eq_ignore_ascii_case(
                                    item["user"]["login"].as_str().unwrap_or(""),
                                )
                            })
                        })
                    {
                        continue;
                    }
                }
                if a["filters"]["ignore_drafts"] == true
                    && kind == "pull_request"
                    && item["draft"] == true
                {
                    continue;
                }
                let selected = a["filters"]["labels"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if !selected.is_empty()
                    && !item["labels"].as_array().is_some_and(|labels| {
                        labels.iter().any(|label| {
                            selected.iter().any(|x| {
                                x.as_str().is_some_and(|n| {
                                    n.eq_ignore_ascii_case(
                                        label["name"].as_str().or(label.as_str()).unwrap_or(""),
                                    )
                                })
                            })
                        })
                    })
                {
                    continue;
                }
                let Some(number) = item["number"].as_u64() else {
                    continue;
                };
                let mut trigger = base.clone();
                trigger["kind"] = json!(kind);
                trigger["number"] = json!(number);
                if kind == "pull_request" {
                    trigger["branch"] = item["head"]["ref"].clone();
                }
                if routes::run_queue(state.clone(), id.to_owned(), trigger)
                    .await
                    .is_ok()
                {
                    count += 1
                } else {
                    queue_failed = true
                }
            }
            if !queue_failed {
                save_mark(state, id, repo, &mark).await?
            }
        }
    }
    Ok(count)
}
async fn save_mark(
    state: &AppState,
    automation: &str,
    repo: &str,
    mark: &str,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO scheduler_marks_v2(automation_id,repository,last_slot) VALUES(?,?,?) ON CONFLICT(automation_id,repository) DO UPDATE SET last_slot=excluded.last_slot").bind(automation).bind(repo).bind(mark).execute(&state.db).await?;
    Ok(())
}
