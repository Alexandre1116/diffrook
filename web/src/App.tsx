import { useCallback, useEffect, useRef, useState } from "react";
import Plans, { type PlanCatalog } from "./Plans";
import {
  Activity,
  ArrowDownRight,
  ArrowRight,
  Bot,
  BellRing,
  Boxes,
  CalendarClock,
  Check,
  CheckCircle2,
  CircleHelp,
  Clock3,
  Code2,
  Command,
  Eye,
  EyeOff,
  ExternalLink,
  FileClock,
  GitBranch,
  GitPullRequest,
  Github,
  KeyRound,
  LayoutDashboard,
  LoaderCircle,
  LogOut,
  Menu,
  MessageCircle,
  MessageSquareText,
  MoreHorizontal,
  Play,
  Plus,
  Radio,
  RefreshCw,
  Search,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  Terminal,
  Trash2,
  TriangleAlert,
  Webhook,
  X,
  XCircle,
} from "lucide-react";

type Page =
  | "overview"
  | "automations"
  | "connections"
  | "providers"
  | "notifications"
  | "runs"
  | "settings";
type Obj = Record<string, any>;
type Status = {
  setup_required: boolean;
  authenticated: boolean;
  version: string;
  sso_enabled: boolean;
  local_login_enabled: boolean;
  sso_requires_license: boolean;
  installation_id: string;
};

async function api<T = any>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    credentials: "same-origin",
    headers: {
      ...(body ? { "Content-Type": "application/json" } : {}),
      ...(method === "GET" ? {} : { "X-Diffrook-Request": "1" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data =
    res.status === 204 ? undefined : await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data?.error || `Request failed (${res.status})`);
  return data as T;
}
const emptyTrigger = {
  events: ["pull_request.opened"],
  command: "/diffrook",
  schedule_enabled: false,
  cron: "0 9 * * 1",
  timezone: "UTC",
  schedule_target: "open_pull_requests",
  branch: "main",
};
const emptyLimits = {
  max_files: 200,
  max_file_bytes: 64000,
  max_context_chars: 180000,
  max_output_tokens: 6000,
  timeout_seconds: 600,
  max_fix_files: 10,
};
const blankAutomation = (): Obj => ({
  name: "",
  description: "",
  enabled: true,
  connection_id: "",
  provider_id: "",
  model: "",
  repositories: [],
  trigger: { ...emptyTrigger },
  filters: {
    labels: [],
    ignore_drafts: true,
    allowed_actors: [],
    ignore_paths: ["vendor/**", "dist/**", "**/*.lock"],
  },
  action: "review",
  instructions: "",
  limits: { ...emptyLimits },
  notifications: [],
  fix: { mode: "new_branch", branch_prefix: "diffrook/" },
});
const events = [
  "pull_request.opened",
  "pull_request.synchronize",
  "pull_request.ready_for_review",
  "issues.opened",
  "issues.labeled",
  "issue_comment.command",
];
const nav: { id: Page; label: string; icon: any }[] = [
  { id: "overview", label: "Overview", icon: LayoutDashboard },
  { id: "automations", label: "Automations", icon: Bot },
  { id: "connections", label: "Connections", icon: GitBranch },
  { id: "providers", label: "AI providers", icon: Sparkles },
  { id: "notifications", label: "Notifications", icon: BellRing },
  { id: "runs", label: "Run history", icon: FileClock },
];
const date = (s?: string) =>
  s
    ? new Date(s).toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      })
    : "—";
const count = (n?: number) => (n ?? 0).toLocaleString();
const statusClass = (s = "") => `status status-${s.toLowerCase()}`;

function Mark() {
  return (
    <div className="brand-mark">
      <svg viewBox="0 0 36 36" aria-hidden="true">
        <path d="M18 3.5 29.5 10v13L18 32.5 6.5 26V10L18 3.5Z" />
        <path d="M12 13.5c0-2.1 1.5-3.5 3.6-3.5 1 0 1.9.4 2.4 1.1.6-.7 1.5-1.1 2.5-1.1 2 0 3.5 1.4 3.5 3.5v8.8c0 2.1-1.5 3.5-3.5 3.5-1 0-1.9-.4-2.5-1.1-.5.7-1.4 1.1-2.4 1.1-2.1 0-3.6-1.4-3.6-3.5v-8.8Z" />
        <path d="M18 12v10" />
      </svg>
    </div>
  );
}
function App() {
  const [status, setStatus] = useState<Status | null>(null),
    [page, setPage] = useState<Page>("overview"),
    [data, setData] = useState<{
      dashboard: Obj;
      automations: Obj[];
      connections: Obj[];
      providers: Obj[];
      notificationProviders: Obj[];
      runs: Obj[];
      updates: Obj;
      license: Obj | null;
      catalog: PlanCatalog | null;
    }>({
      dashboard: {},
      automations: [],
      connections: [],
      providers: [],
      notificationProviders: [],
      runs: [],
      updates: { releases: [], policy: "manual" },
      license: null,
      catalog: null,
    });
  const [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [notice, setNotice] = useState(""),
    [mobile, setMobile] = useState(false),
    [accountMenu, setAccountMenu] = useState(false),
    [query, setQuery] = useState("");
  const [modal, setModal] = useState<{ kind: string; item?: Obj } | null>(null),
    [detail, setDetail] = useState<Obj | null>(null);
  const accountMenuRef = useRef<HTMLDivElement>(null);
  const reload = useCallback(async () => {
    if (!status?.authenticated) return;
    setBusy(true);
    try {
      const [dashboard, automations, connections, providers, notificationProviders, runs, updates, license, catalog] =
        await Promise.all([
          api("/dashboard"),
          api("/automations"),
          api("/connections"),
          api("/providers"),
          api("/notification-providers"),
          api("/runs"),
          api("/updates"),
          api("/license"),
          api<PlanCatalog>("/plans"),
        ]);
      setData({ dashboard, automations, connections, providers, notificationProviders, runs, updates, license, catalog });
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }, [status?.authenticated]);
  useEffect(() => {
    if (new URLSearchParams(window.location.search).has("sso_error")) {
      setError(new URLSearchParams(window.location.search).get("sso_error") === "user_limit"
        ? "This installation's user limit has been reached. Contact your administrator about the paid plan's allowance."
        : new URLSearchParams(window.location.search).get("sso_error") === "plan_required"
        ? "SSO requires an active Teams or Enterprise license. Existing identities can still sign in after a downgrade."
        : "SSO sign-in failed. Your account may not be authorized. Try again or contact your administrator.");
      window.history.replaceState({}, "", window.location.pathname);
    }
    api<Status>("/status")
      .then(setStatus)
      .catch((e) => setError(e.message));
  }, []);
  useEffect(() => {
    if (status?.authenticated) void reload();
  }, [status?.authenticated, reload]);
  useEffect(() => {
    if (!status?.authenticated) return;
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void reload();
    }, 5000);
    return () => clearInterval(timer);
  }, [status?.authenticated, reload]);
  useEffect(() => {
    if (!detail || !["queued", "running"].includes(detail.status)) return;
    const timer = setInterval(() => {
      api(`/runs/${detail.id}`).then(setDetail).catch(e => setError(e.message));
    }, 2000);
    return () => clearInterval(timer);
  }, [detail?.id, detail?.status]);
  useEffect(() => {
    if (notice) {
      const t = setTimeout(() => setNotice(""), 3500);
      return () => clearTimeout(t);
    }
  }, [notice]);
  useEffect(() => {
    if (!accountMenu) return;
    const closeOnOutsideClick = (event: PointerEvent) => {
      if (!accountMenuRef.current?.contains(event.target as Node))
        setAccountMenu(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setAccountMenu(false);
    };
    document.addEventListener("pointerdown", closeOnOutsideClick);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsideClick);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [accountMenu]);
  const runDetail = async (r: Obj) => {
    try {
      setDetail(await api(`/runs/${r.id}`));
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const mutate = async (path: string, method: string, body?: Obj) => {
    setError("");
    try {
      await api(path, method, body);
      setModal(null);
      setNotice(
        method === "DELETE" ? "Deleted successfully" : "Saved successfully",
      );
      await reload();
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const createAutomation = () => {
    if (data.license?.can_create_automation === false) {
      setError("Automation limit reached. Delete an automation or upgrade your self-hosted plan. See Settings for plan details.");
      return;
    }
    setModal({ kind: "automation", item: blankAutomation() });
  };
  const login = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const fd = new FormData(e.currentTarget);
    setBusy(true);
    setError("");
    try {
      await api(
        status?.setup_required ? "/setup" : "/login",
        "POST",
        Object.fromEntries(fd),
      );
      setStatus(await api("/status"));
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const signOut = async () => {
    try {
      await api("/logout", "POST");
      setAccountMenu(false);
      setStatus(await api("/status"));
    } catch (e) {
      setError((e as Error).message);
    }
  };
  if (!status)
    return (
      <div className="splash">
        <Mark />
        <LoaderCircle className="spin" size={18} />
        Connecting to Diffrook
      </div>
    );
  if (!status.authenticated)
    return (
      <Auth
        setup={status.setup_required}
        sso={status.sso_enabled}
        localLogin={status.local_login_enabled}
        ssoRequiresLicense={status.sso_requires_license}
        installationId={status.installation_id}
        error={error}
        busy={busy}
        submit={login}
      />
    );
  const pageTitle = nav.find((n) => n.id === page)?.label || "Settings";
  return (
    <div className="shell">
      <aside className={`sidebar ${mobile ? "open" : ""}`}>
        <div className="brand">
          <Mark />
          <div>
            <strong>diffrook</strong>
            <span>REPOSITORY AUTOMATION</span>
          </div>
          <button className="mobile-close" onClick={() => setMobile(false)}>
            <X size={18} />
          </button>
        </div>
        <div className="workspace">
          <span className="workspace-icon">
            <Code2 size={16} />
          </span>
          <span>
            <b>Workspace</b>
            <small>Self-hosted instance</small>
          </span>
        </div>
        <div className="nav-label">WORKSPACE</div>
        <nav>
          {nav.map((n) => (
            <button
              key={n.id}
              className={`nav-item ${page === n.id ? "selected" : ""}`}
              onClick={() => {
                setPage(n.id);
                setMobile(false);
              }}
            >
              <n.icon size={17} />
              <span>{n.label}</span>
              {n.id === "runs" && (
                <span className="nav-count">{data.runs.length || ""}</span>
              )}
            </button>
          ))}
        </nav>
        <div className="side-bottom">
          <div className="side-status">
            <span className="live-dot" />
            Instance reachable<span className="version">v{status.version}</span>
          </div>
          <div className="account-wrap" ref={accountMenuRef}>
            {accountMenu && (
              <div className="account-menu" id="account-menu">
                <button onClick={() => {
                  setPage("settings");
                  setAccountMenu(false);
                  setMobile(false);
                }}>
                  <Settings2 size={16} />
                  Settings
                </button>
                <button onClick={() => void signOut()}>
                  <LogOut size={16} />
                  Sign out
                </button>
              </div>
            )}
            <div className="user-row">
              <div className="avatar">A</div>
              <span>
                <b>Administrator</b>
                <small>Local account</small>
              </span>
              <button
                className="account-menu-toggle"
                aria-label="Account menu"
                aria-haspopup="menu"
                aria-controls="account-menu"
                aria-expanded={accountMenu}
                onClick={() => setAccountMenu((open) => !open)}
              >
                <MoreHorizontal size={18} />
              </button>
            </div>
          </div>
        </div>
      </aside>
      {mobile && (
        <button
          className="scrim"
          onClick={() => setMobile(false)}
          aria-label="Close menu"
        />
      )}
      <main className="main">
        <header className="topbar">
          <button
            className="icon-btn mobile-menu"
            onClick={() => setMobile(true)}
            aria-label="Open menu"
          >
            <Menu size={19} />
          </button>
          <div className="breadcrumbs">
            <span>Diffrook</span>
            <span className="crumb-slash">/</span>
            <strong>{pageTitle}</strong>
          </div>
          <div className="top-actions">
            <div className="instance-pill">
              <span className="live-dot" />
              API connected
            </div>
          </div>
        </header>
        <div className="content">
          {error && (
            <div className="alert error">
              <TriangleAlert size={17} />
              <span>{error}</span>
              <button onClick={() => setError("")}>
                <X size={15} />
              </button>
            </div>
          )}
          {notice && (
            <div className="alert success">
              <CheckCircle2 size={17} />
              {notice}
            </div>
          )}
          {page === "overview" && (
            <Overview
              data={data}
              busy={busy}
              go={setPage}
              openRun={runDetail}
              refresh={reload}
              version={status.version}
              updates={data.updates}
              updatePolicy={async (mode) => {
                try {
                  await api("/updates/policy", "POST", { mode });
                  await reload();
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
              applyUpdate={async (tag) => {
                try {
                  await api("/updates/apply", "POST", { tag });
                  setNotice("Update installed. Reconnecting to Diffrook…");
                  window.setTimeout(() => window.location.reload(), 2500);
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            />
          )}
          {page === "automations" && (
            <AutomationPage
              items={data.automations}
              connections={data.connections}
              providers={data.providers}
              canCreate={data.license?.can_create_automation !== false}
              query={query}
              setQuery={setQuery}
              create={createAutomation}
              edit={(item) => setModal({ kind: "automation", item })}
              run={(item) => setModal({ kind: "run", item })}
              remove={(item) =>
                void mutate(`/automations/${item.id}`, "DELETE")
              }
            />
          )}
          {page === "connections" && (
            <ResourcePage
              kind="connection"
              items={data.connections}
              query={query}
              setQuery={setQuery}
              create={() =>
                setModal({
                  kind: "connection",
                  item: {
                    name: "",
                    kind: "github",
                    base_url: "https://api.github.com",
                    token: "",
                    webhook_secret: "",
                    bot_username: "",
                  },
                })
              }
              edit={(item) => setModal({ kind: "connection", item })}
              remove={(item) =>
                void mutate(`/connections/${item.id}`, "DELETE")
              }
              test={async (item) => {
                try {
                  const r = await api(
                    `/connections/${item.id}/test`,
                    "POST",
                    {},
                  );
                  r.ok ? setNotice(r.message) : setError(r.message);
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            />
          )}
          {page === "providers" && (
            <ResourcePage
              kind="provider"
              items={data.providers}
              query={query}
              setQuery={setQuery}
              create={() =>
                setModal({
                  kind: "provider",
                  item: {
                    name: "",
                    kind: "openai",
                    base_url: "https://api.openai.com/v1",
                    api_key: "",
                    default_model: "",
                  },
                })
              }
              edit={(item) => setModal({ kind: "provider", item })}
              remove={(item) => void mutate(`/providers/${item.id}`, "DELETE")}
              test={async (item) => {
                try {
                  const r = await api(`/providers/${item.id}/test`, "POST", {});
                  r.ok ? setNotice(r.message) : setError(r.message);
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            />
          )}
          {page === "notifications" && (
            <ResourcePage
              kind="notification"
              items={data.notificationProviders}
              query={query}
              setQuery={setQuery}
              create={() => setModal({
                kind: "notification",
                item: { name: "", kind: "discord", url: "" },
              })}
              edit={(item) => setModal({ kind: "notification", item })}
              remove={(item) => void mutate(`/notification-providers/${item.id}`, "DELETE")}
              test={async (item) => {
                try {
                  const r = await api(`/notification-providers/${item.id}/test`, "POST", {});
                  r.ok ? setNotice(r.message) : setError(r.message);
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            />
          )}
          {page === "runs" && (
            <RunsPage
              items={data.runs}
              query={query}
              setQuery={setQuery}
              open={runDetail}
              refresh={reload}
              cancel={async (r) => {
                try {
                  await api(`/runs/${r.id}/cancel`, "POST", {});
                  setNotice("Cancellation requested");
                  await reload();
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
              retry={async (r) => {
                try {
                  await api(`/runs/${r.id}/retry`, "POST", {});
                  setNotice("Run queued again");
                  await reload();
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            />
          )}
          {page === "settings" && (
            <SettingsPage data={data} notice={setNotice} />
          )}
        </div>
      </main>
      {modal && (
        <Modal
          title={
            modal.kind === "automation"
              ? modal.item?.id
                ? "Edit automation"
                : "New automation"
              : modal.kind === "connection"
                ? modal.item?.id
                  ? "Edit connection"
                  : "Add connection"
                : modal.kind === "provider"
                  ? modal.item?.id
                    ? "Edit AI provider"
                    : "Add AI provider"
                  : modal.kind === "notification"
                    ? modal.item?.id
                      ? "Edit notification destination"
                      : "Add notification destination"
                    : "Run automation"
          }
          close={() => setModal(null)}
          wide={modal.kind === "automation"}
        >
          {error && <div className="alert error" role="alert">{error}</div>}
          {modal.kind === "automation" ? (
            <AutomationForm
              item={modal.item!}
              connections={data.connections}
              providers={data.providers}
              notificationProviders={data.notificationProviders}
              save={(v) =>
                void mutate(
                  `/automations${v.id ? `/${v.id}` : ""}`,
                  v.id ? "PUT" : "POST",
                  v,
                )
              }
              cancel={() => setModal(null)}
            />
          ) : modal.kind === "connection" || modal.kind === "provider" || modal.kind === "notification" ? (
            <ResourceForm
              kind={modal.kind}
              item={modal.item!}
              save={(v) =>
                void mutate(
                  `/${modal.kind === "connection" ? "connections" : modal.kind === "provider" ? "providers" : "notification-providers"}${v.id ? `/${v.id}` : ""}`,
                  v.id ? "PUT" : "POST",
                  v,
                )
              }
              cancel={() => setModal(null)}
            />
          ) : modal.kind.startsWith("delete-") ? (
            <div className="form-stack">
              <p className="delete-copy">
                Delete <b>{modal.item?.name}</b>? This action cannot be undone.
              </p>
              <div className="form-actions">
                <button
                  className="btn secondary"
                  onClick={() => setModal(null)}
                >
                  Keep it
                </button>
                <button
                  className="btn delete-btn"
                  onClick={() =>
                    void mutate(
                      `/${modal.kind.slice(7)}/${modal.item?.id}`,
                      "DELETE",
                    )
                  }
                >
                  <Trash2 size={15} />
                  Delete
                </button>
              </div>
            </div>
          ) : (
            <RunForm
              item={modal.item!}
              submit={async (trigger) => {
                try {
                  await api(
                    `/automations/${modal.item!.id}/run`,
                    "POST",
                    trigger,
                  );
                  setModal(null);
                  setPage("runs");
                  setNotice("Run queued");
                  await reload();
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
              cancel={() => setModal(null)}
            />
          )}
        </Modal>
      )}
      {detail && <RunDetail run={detail} close={() => setDetail(null)} />}
    </div>
  );
}
function Auth({
  setup,
  sso,
  localLogin,
  ssoRequiresLicense,
  installationId,
  error,
  busy,
  submit,
}: {
  setup: boolean;
  sso: boolean;
  localLogin: boolean;
  ssoRequiresLicense: boolean;
  installationId: string;
  error: string;
  busy: boolean;
  submit: (e: React.FormEvent<HTMLFormElement>) => void;
}) {
  const [show, setShow] = useState(false);
  return (
    <div className="auth">
      <div className="auth-card">
        <div className="auth-brand">
          <Mark />
          <span>diffrook</span>
        </div>
        <div className="auth-kicker">SELF-HOSTED REPOSITORY AUTOMATION</div>
        <h1>{setup ? "Set up your workspace" : "Welcome back"}</h1>
        <p>
          {setup
            ? "Create the administrator account for this Diffrook instance."
            : "Sign in to manage your repository automations."}
        </p>
        {sso && (
          <a className="btn primary full" href="/api/auth/oidc/start">
            <ShieldCheck size={17} /> Sign in with SSO
          </a>
        )}
        {ssoRequiresLicense && <div className="auth-license-note" role="status">
          <p>SSO requires an active Teams or Enterprise license. Ask the instance administrator to install a signed license.</p>
          {!localLogin && <p>Installation ID: <code>{installationId}</code></p>}
          <a className="text-btn" href="https://github.com/Alexandre1116/diffrook/blob/main/docs/licensing.md" target="_blank" rel="noreferrer">License instructions <ExternalLink size={13} /></a>
        </div>}
        {sso && localLogin && <p className="auth-separator">Or use your local administrator account</p>}
        {localLogin && <form onSubmit={submit} className="form-stack">
          {setup && (
            <label>
              Setup token
              <input
                name="setup_token"
                type="password"
                placeholder="Paste the token from your server logs"
                required
                autoComplete="off"
              />
            </label>
          )}
          <label>
            Username
            <input
              name="username"
              required
              autoComplete="username"
              placeholder="admin"
            />
          </label>
          <label>
            Password
            <div className="password-wrap">
              <input
                name="password"
                type={show ? "text" : "password"}
                required
                minLength={setup ? 12 : 1}
                autoComplete={setup ? "new-password" : "current-password"}
                placeholder={setup ? "At least 12 characters" : "Your password"}
              />
              <button
                type="button"
                onClick={() => setShow(!show)}
                aria-label="Toggle password visibility"
              >
                {show ? <EyeOff size={17} /> : <Eye size={17} />}
              </button>
            </div>
          </label>
          {error && (
            <div className="form-error">
              <TriangleAlert size={16} />
              {error}
            </div>
          )}
          <button className="btn primary full" disabled={busy}>
            {busy ? (
              <LoaderCircle className="spin" size={17} />
            ) : setup ? (
              <KeyRound size={16} />
            ) : (
              <ArrowRight size={16} />
            )}{" "}
            {setup ? "Create administrator" : "Sign in"}
          </button>
        </form>}
        {!localLogin && error && <div className="form-error" role="alert"><TriangleAlert size={16} />{error}</div>}
        <div className="auth-foot">
          <ShieldCheck size={15} /> Your instance. Your repositories. Your
          rules.
        </div>
      </div>
      <div className="auth-version">DIFFROOK · PRIVATE BY DESIGN</div>
    </div>
  );
}

function Overview({
  data,
  busy,
  go,
  openRun,
  refresh,
  version,
  updates,
  updatePolicy,
  applyUpdate,
}: {
  data: any;
  busy: boolean;
  go: (p: Page) => void;
  openRun: (r: Obj) => void;
  refresh: () => void;
  version: string;
  updates: Obj;
  updatePolicy: (mode: string) => Promise<void>;
  applyUpdate: (tag: string) => Promise<void>;
}) {
  const d = data.dashboard;
  const latest = (d.recent_runs || data.runs).slice(0, 5);
  const [updating, setUpdating] = useState(false);
  const release = (updates.releases || []).find((r: Obj) => r.newer);
  return (
    <>
      <div className="page-head">
        <div>
          <div className="eyebrow">YOUR REPOSITORY OPERATIONS</div>
          <h1>Overview</h1>
          <p>A clear view of what your automations are doing.</p>
        </div>
        <div className="head-buttons">
          <button className="btn secondary" onClick={refresh}>
            <RefreshCw size={15} className={busy ? "spin" : ""} />
            Refresh
          </button>
          <button className="btn primary" onClick={() => go("automations")}>
            <Plus size={16} />
            New automation
          </button>
        </div>
      </div>
      <div className="welcome-banner">
        <div className="banner-icon">
          <Mark />
        </div>
        <div>
          <span className="banner-eyebrow">DIFFROOK IS READY</span>
          <b>Good code deserves a second set of eyes.</b>
          <p>
            Connect a repository and an AI provider to start your first
            automated review.
          </p>
        </div>
        <button className="banner-link" onClick={() => go("connections")}>
          Set up connections <ArrowRight size={16} />
        </button>
        <span className="banner-line" />
      </div>
      <div className="stat-grid">
        <Stat
          label="Automations"
          value={d.automations}
          hint={`${d.active_automations ?? 0} active`}
          icon={Bot}
        />
        <Stat label="Runs" value={d.runs} hint="All time" icon={Activity} />
        <Stat
          label="Findings"
          value={d.findings}
          hint="Across all runs"
          icon={TriangleAlert}
        />
        <div className="stat-card uptime">
          <div className="stat-top">
            <span>INSTANCE STATUS</span>
            <ShieldCheck size={17} />
          </div>
          <strong>
            <i className="live-dot" />
            API reachable
          </strong>
          <small>Version {version}</small>
        </div>
      </div>
      <section className="panel-card release-panel">
        <div className="release-copy">
          <div className="eyebrow">DIFFROOK RELEASES</div>
          <h2>{release ? `Version ${release.version} is available` : "Version updates"}</h2>
          <p>{release ? `${release.prerelease ? "Pre-release" : "Release"} published ${date(release.published_at)}. ${release.supported ? "A compatible Linux package is ready." : "No update package for this platform yet."}` : `You are running the latest version, ${version}.`}</p>
          <div className="release-list">
            {(updates.releases || []).slice(0, 4).map((item: Obj) => <a key={item.tag} href={item.url} target="_blank" rel="noreferrer">{item.version}{item.prerelease && <span>Pre-release</span>}</a>)}
            {!(updates.releases || []).length && <small>{updates.error || "Checking GitHub for published releases…"}</small>}
          </div>
        </div>
        <div className="release-controls">
          <label>Update policy
            <select value={updates.policy || "manual"} onChange={(e) => void updatePolicy(e.target.value)}>
              <option value="manual">Manual updates</option>
              <option value="automatic">Automatic updates</option>
            </select>
          </label>
          {release && <button className="btn primary" disabled={updating || !release.supported} onClick={async () => { setUpdating(true); try { await applyUpdate(release.tag); } finally { setUpdating(false); } }}><ArrowDownRight size={15} />{updating ? "Updating…" : "Install update"}</button>}
          <small>Automatic updates download and restart this Linux container. Back up /data first.</small>
        </div>
      </section>
      <div className="section-heading">
        <div>
          <h2>Recent runs</h2>
          <p>Latest activity across your automations</p>
        </div>
        <button className="text-btn" onClick={() => go("runs")}>
          View all <ArrowRight size={15} />
        </button>
      </div>
      <div className="table-card">
        <RunTable items={latest} open={openRun} />
        {!latest.length && (
          <Empty
            icon={FileClock}
            title="No runs yet"
            body="When an automation runs, its results will show up here."
            action="Go to automations"
            onAction={() => go("automations")}
          />
        )}
      </div>
      <div className="bottom-grid">
        <div className="panel-card setup-card">
          <div className="panel-title">
            <div>
              <h3>Get started</h3>
              <p>Three steps to your first review</p>
            </div>
            <span className="progress-chip">
              {
                [
                  data.connections.length > 0,
                  data.providers.length > 0,
                  data.automations.length > 0,
                ].filter(Boolean).length
              }{" "}
              / 3
            </span>
          </div>
          <div className="steps">
            {[
              {
                ok: data.connections.length > 0,
                label: "Connect a code host",
                sub: "GitHub or Forgejo",
                go: "connections" as Page,
                icon: GitBranch,
              },
              {
                ok: data.providers.length > 0,
                label: "Add an AI provider",
                sub: "OpenAI, Anthropic or Ollama",
                go: "providers" as Page,
                icon: Sparkles,
              },
              {
                ok: data.automations.length > 0,
                label: "Create an automation",
                sub: "Choose repositories and actions",
                go: "automations" as Page,
                icon: Bot,
              },
            ].map((s, i) => (
              <button
                className={`step ${s.ok ? "done" : ""}`}
                key={s.label}
                onClick={() => go(s.go)}
              >
                <span className="step-num">
                  {s.ok ? <Check size={14} /> : `0${i + 1}`}
                </span>
                <span className="step-icon">
                  <s.icon size={16} />
                </span>
                <span className="step-copy">
                  <b>{s.label}</b>
                  <small>{s.sub}</small>
                </span>
                {s.ok ? (
                  <CheckCircle2 size={17} className="step-check" />
                ) : (
                  <ArrowRight size={16} className="step-arrow" />
                )}
              </button>
            ))}
          </div>
        </div>
        <div className="panel-card quick-card">
          <div className="panel-title">
            <div>
              <h3>Quick links</h3>
              <p>Manage your setup</p>
            </div>
            <div className="quick-spark">
              <Command size={16} />
            </div>
          </div>
          <button onClick={() => go("automations")}>
            <Bot size={17} />
            <span>
              <b>Automations</b>
              <small>Configure your workflows</small>
            </span>
            <ArrowRight size={15} />
          </button>
          <button onClick={() => go("connections")}>
            <Github size={17} />
            <span>
              <b>Connections</b>
              <small>Manage code host access</small>
            </span>
            <ArrowRight size={15} />
          </button>
          <button onClick={() => go("settings")}>
            <SlidersHorizontal size={17} />
            <span>
              <b>Settings</b>
              <small>Export configuration</small>
            </span>
            <ArrowRight size={15} />
          </button>
        </div>
      </div>
    </>
  );
}
function Stat({
  label,
  value,
  hint,
  icon: Icon,
}: {
  label: string;
  value: number | undefined;
  hint: string;
  icon: any;
}) {
  return (
    <div className="stat-card">
      <div className="stat-top">
        <span>{label.toUpperCase()}</span>
        <Icon size={17} />
      </div>
      <strong>{count(value)}</strong>
      <small>{hint}</small>
    </div>
  );
}
function RunTable({ items, open }: { items: Obj[]; open: (r: Obj) => void }) {
  return (
    <table className="data-table">
      <thead>
        <tr>
          <th>RUN</th>
          <th>TRIGGER</th>
          <th>STATUS</th>
          <th>STARTED</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {items.map((r) => (
          <tr key={r.id} onClick={() => open(r)}>
            <td>
              <span className="run-title">
                <span className="run-glyph">
                  <GitPullRequest size={15} />
                </span>
                <span>
                  <b>{r.automation_name || "Automation run"}</b>
                  <small>
                    {r.trigger?.repository || r.trigger?.kind || "Manual run"}
                    {r.trigger?.number ? ` #${r.trigger.number}` : ""}
                  </small>
                </span>
              </span>
            </td>
            <td>
              <span className="trigger-label">
                {r.trigger?.kind === "pull_request"
                  ? "Pull request"
                  : r.trigger?.kind === "issue"
                    ? "Issue"
                    : r.trigger?.kind || "Schedule"}
              </span>
            </td>
            <td>
              <span className={statusClass(r.status)}>
                <i />
                {r.status}
              </span>
            </td>
            <td className="muted-cell">{date(r.started_at || r.created_at)}</td>
            <td>
              <ArrowRight size={15} className="row-arrow" />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
function Empty({
  icon: Icon,
  title,
  body,
  action,
  onAction,
}: {
  icon: any;
  title: string;
  body: string;
  action?: string;
  onAction?: () => void;
}) {
  return (
    <div className="empty">
      <div className="empty-icon">
        <Icon size={21} />
      </div>
      <h3>{title}</h3>
      <p>{body}</p>
      {action && (
        <button className="btn secondary" onClick={onAction}>
          {action}
          <ArrowRight size={15} />
        </button>
      )}
    </div>
  );
}

function AutomationPage({
  items,
  connections,
  providers,
  query,
  setQuery,
  create,
  canCreate,
  edit,
  run,
  remove,
}: {
  items: Obj[];
  connections: Obj[];
  providers: Obj[];
  query: string;
  setQuery: (s: string) => void;
  create: () => void;
  canCreate: boolean;
  edit: (o: Obj) => void;
  run: (o: Obj) => void;
  remove: (o: Obj) => void;
}) {
  const filtered = items.filter((x) =>
    `${x.name} ${x.repositories?.join(" ")}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  const cn = (id: string) =>
    connections.find((x) => x.id === id)?.name || "Connection missing";
  const pn = (id: string) =>
    providers.find((x) => x.id === id)?.name || "Provider missing";
  return (
    <>
      <div className="page-head">
        <div>
          <div className="eyebrow">WORKFLOWS</div>
          <h1>Automations</h1>
          <p>
            Define when Diffrook should inspect or update your repositories.
          </p>
        </div>
        <button className="btn primary" onClick={create} disabled={!canCreate} title={!canCreate ? "Plan limit reached. See Settings." : undefined}>
          <Plus size={16} />
          New automation
        </button>
      </div>
      {!canCreate && <div className="notice-strip"><ShieldCheck size={17} /><span>Your plan's automation limit is reached. Disabled automations also count. Delete one or upgrade your self-hosted plan. Plan details are in Settings.</span></div>}
      <div className="toolbar">
        <div className="searchbox">
          <Search size={16} />
          <input
            placeholder="Search automations or repositories"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          {query && (
            <button onClick={() => setQuery("")}>
              <X size={14} />
            </button>
          )}
        </div>
        <div className="toolbar-meta">
          <span>{items.length} total</span>
          <SlidersHorizontal size={16} />
        </div>
      </div>
      {filtered.length ? (
        <div className="automation-grid">
          {filtered.map((a) => (
            <article className="automation-card" key={a.id}>
              <div className="automation-card-top">
                <span className={`automation-symbol action-${a.action}`}>
                  <Bot size={19} />
                </span>
                <div className="automation-name">
                  <h3>{a.name}</h3>
                  <span>
                    {a.enabled ? (
                      <>
                        <i className="live-dot" />
                        Enabled
                      </>
                    ) : (
                      <>
                        <i className="off-dot" />
                        Paused
                      </>
                    )}
                  </span>
                </div>
                <button
                  className="icon-btn more"
                  onClick={() => edit(a)}
                  aria-label="Edit automation"
                >
                  <MoreHorizontal size={19} />
                </button>
              </div>
              <p className="automation-desc">
                {a.description || "No description added."}
              </p>
              <div className="action-pill">
                <span>Action</span>
                <b>{actionLabel(a.action)}</b>
                <span className="pill-sep">·</span>
                {a.model || "Default model"}
              </div>
              <div className="repo-list">
                {(a.repositories || []).length ? (
                  (a.repositories || []).slice(0, 3).map((r: string) => (
                    <span className="repo-chip" key={r}>
                      <Code2 size={13} />
                      {r}
                    </span>
                  ))
                ) : (
                  <span className="subtle">No repositories selected</span>
                )}
                {(a.repositories || []).length > 3 && (
                  <span className="repo-chip">
                    +{a.repositories.length - 3}
                  </span>
                )}
              </div>
              <div className="automation-meta">
                <span>
                  <GitBranch size={14} />
                  {cn(a.connection_id)}
                </span>
                <span>
                  <Sparkles size={14} />
                  {pn(a.provider_id)}
                </span>
              </div>
              <div className="automation-card-foot">
                <span>
                  {a.trigger?.schedule_enabled ? (
                    <>
                      <CalendarClock size={14} />
                      {a.trigger.cron} · {a.trigger.timezone}
                    </>
                  ) : (
                    <>
                      <Radio size={14} />
                      {a.trigger?.events?.length || 0} event triggers
                    </>
                  )}
                </span>
                <div>
                  <button
                    className="btn small secondary"
                    onClick={() => run(a)}
                  >
                    <Play size={13} />
                    Run now
                  </button>
                  <button
                    className="icon-btn"
                    aria-label="Delete automation"
                    onClick={() => remove(a)}
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              </div>
            </article>
          ))}
        </div>
      ) : (
        <div className="table-card">
          <Empty
            icon={Bot}
            title={query ? "No matching automations" : "No automations yet"}
            body={
              query
                ? "Try another name or repository."
                : "Create a workflow to review pull requests, fix issues, or audit a branch."
            }
            action={query ? undefined : "Create your first automation"}
            onAction={create}
          />
        </div>
      )}
    </>
  );
}
function actionLabel(a: string) {
  return (
    (
      {
        review: "Review & comment",
        fix_pr: "Fix pull requests",
        solve_issue: "Solve issues",
        audit: "Repository audit",
      } as Record<string, string>
    )[a] || a
  );
}
function ResourcePage({
  kind,
  items,
  query,
  setQuery,
  create,
  edit,
  remove,
  test,
}: {
  kind: "connection" | "provider" | "notification";
  items: Obj[];
  query: string;
  setQuery: (s: string) => void;
  create: () => void;
  edit: (o: Obj) => void;
  remove: (o: Obj) => void;
  test: (o: Obj) => void;
}) {
  const isConn = kind === "connection";
  const isNotif = kind === "notification";
  const filtered = items.filter((x) =>
    `${x.name} ${x.kind} ${x.base_url}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  return (
    <>
      <div className="page-head">
        <div>
          <div className="eyebrow">
            {isConn ? "SOURCE CONTROL" : isNotif ? "DELIVERY CHANNELS" : "MODEL ACCESS"}
          </div>
          <h1>{isConn ? "Connections" : isNotif ? "Notification providers" : "AI providers"}</h1>
          <p>
            {isConn
              ? "Connect GitHub or Forgejo so Diffrook can read and act on repository events."
              : isNotif
                ? "Configure reusable Discord, Slack, Teams, or webhook destinations for your automations."
                : "Choose the models that power reviews, issue fixes, and audits."}
          </p>
        </div>
        <button className="btn primary" onClick={create}>
          <Plus size={16} />
          {isConn ? "Add connection" : isNotif ? "Add destination" : "Add provider"}
        </button>
      </div>
      <div className="notice-strip">
        <ShieldCheck size={17} />
        <span>
          <b>Secrets are encrypted at rest.</b>{" "}
          {isConn ? "Tokens and webhook secrets" : isNotif ? "Webhook URLs" : "API keys"} are never shown
          again after saving. Leave a secret field empty to keep its current
          value.
        </span>
      </div>
      <div className="toolbar">
        <div className="searchbox">
          <Search size={16} />
          <input
            placeholder={`Search ${isConn ? "connections" : isNotif ? "notification destinations" : "providers"}`}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <div className="toolbar-meta">{items.length} configured</div>
      </div>
      {filtered.length ? (
        <div className="resource-list">
          {filtered.map((item) => (
            <article className="resource-card" key={item.id}>
              <div className={`resource-logo ${item.kind}`}>
                {isConn ? (
                  item.kind === "github" ? (
                    <Github size={20} />
                  ) : (
                    <GitBranch size={20} />
                  )
                ) : isNotif ? (
                  item.kind === "discord" ? <MessageCircle size={20} /> : item.kind === "slack" ? <MessageSquareText size={20} /> : item.kind === "webhook" ? <Webhook size={20} /> : <BellRing size={20} />
                ) : (
                  <Sparkles size={20} />
                )}
              </div>
              <div className="resource-info">
                <div className="resource-title">
                  <h3>{item.name}</h3>
                  <span className="kind-badge">{item.kind}</span>
                </div>
                <p>{isNotif ? (item.has_url ? "Webhook URL securely saved" : "No webhook URL saved") : item.base_url || "Default endpoint"}</p>
                <div className="resource-details">
                  {isConn ? (
                    <>
                      <span>
                        <KeyRound size={13} />
                        {item.has_token ? "Token saved" : "No token saved"}
                      </span>
                      {item.bot_username && (
                        <span>Bot: {item.bot_username}</span>
                      )}
                      {item.has_webhook_secret && (
                        <span>
                          <ShieldCheck size={13} />
                          Webhook secret saved
                        </span>
                      )}
                    </>
                  ) : isNotif ? (
                    <span>
                      <KeyRound size={13} />
                      {item.has_url ? "Destination URL saved" : "URL required"}
                    </span>
                  ) : (
                    <>
                      <span>
                        <Sparkles size={13} />
                        {item.default_model || "No default model"}
                      </span>
                      <span>
                        <KeyRound size={13} />
                        {item.has_api_key
                          ? "API key saved"
                          : "No API key saved"}
                      </span>
                    </>
                  )}
                </div>
              </div>
              <div className="resource-actions">
                <button
                  className="btn secondary small"
                  onClick={() => test(item)}
                >
                  <Radio size={14} />
                  {isNotif ? "Send test" : "Test connection"}
                </button>
                <button
                  className="icon-btn"
                  onClick={() => edit(item)}
                  aria-label="Edit"
                >
                  <Settings2 size={17} />
                </button>
                <button
                  className="icon-btn danger-icon"
                  onClick={() => remove(item)}
                  aria-label="Delete"
                >
                  <Trash2 size={16} />
                </button>
              </div>
            </article>
          ))}
        </div>
      ) : (
        <div className="table-card">
          <Empty
            icon={isConn ? GitBranch : Sparkles}
            title={
              query
              ? "No matching records"
              : isConn ? "No connections yet" : isNotif ? "No notification destinations yet" : "No providers yet"
            }
            body={
              isConn
                ? "Add a code host to give automations access to your repositories."
                : isNotif
                  ? "Add a destination once, then reuse it across any automation. Webhook URLs are encrypted at rest."
                  : "Add a provider to choose the model used by your automations."
            }
            action={
              query
                ? undefined
                : isConn
                  ? "Add your first connection"
                  : isNotif ? "Add your first destination" : "Add your first provider"
            }
            onAction={create}
          />
        </div>
      )}
    </>
  );
}

function RunsPage({
  items,
  query,
  setQuery,
  open,
  refresh,
  cancel,
  retry,
}: {
  items: Obj[];
  query: string;
  setQuery: (s: string) => void;
  open: (r: Obj) => void;
  refresh: () => void;
  cancel: (r: Obj) => void;
  retry: (r: Obj) => void;
}) {
  const filtered = items.filter((r) =>
    `${r.automation_name} ${r.trigger?.repository} ${r.status}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  return (
    <>
      <div className="page-head">
        <div>
          <div className="eyebrow">EXECUTION LOG</div>
          <h1>Run history</h1>
          <p>Inspect results, findings, and errors from automation runs.</p>
        </div>
        <button className="btn secondary" onClick={refresh}>
          <RefreshCw size={15} />
          Refresh
        </button>
      </div>
      <div className="toolbar">
        <div className="searchbox">
          <Search size={16} />
          <input
            placeholder="Filter by automation, repository, or status"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <span className="toolbar-meta">{filtered.length} runs</span>
      </div>
      <div className="table-card">
        {filtered.length ? (
          <>
            <RunTable items={filtered} open={open} />
            <div className="run-actions-list">
              {filtered.slice(0, 30).map((r) => (
                <span key={r.id}>
                  {r.status === "running" || r.status === "queued" ? (
                    <button onClick={() => cancel(r)}>
                      <XCircle size={14} />
                      Cancel
                    </button>
                  ) : null}
                  {r.status === "failed" || r.status === "cancelled" ? (
                    <button onClick={() => retry(r)}>
                      <RefreshCw size={14} />
                      Retry
                    </button>
                  ) : null}
                </span>
              ))}
            </div>
          </>
        ) : (
          <Empty
            icon={FileClock}
            title={query ? "No runs match" : "Nothing has run yet"}
            body={
              query
                ? "Adjust your filters and try again."
                : "Runs will appear here after an automation is triggered."
            }
          />
        )}
      </div>
    </>
  );
}

function SettingsPage({
  data,
  notice,
}: {
  data: any;
  notice: (s: string) => void;
}) {
  const exportConfig = () => {
    const sanitized = {
      automations: data.automations,
      connections: data.connections,
      providers: data.providers,
      notification_providers: data.notificationProviders,
    };
    const blob = new Blob([JSON.stringify(sanitized, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "diffrook-config.json";
    a.click();
    URL.revokeObjectURL(url);
    notice("Configuration export downloaded");
  };
  return (
    <>
      <div className="page-head">
        <div>
          <div className="eyebrow">INSTANCE</div>
          <h1>Settings</h1>
          <p>Manage this instance and export its configuration.</p>
        </div>
      </div>
      <div className="settings-grid">
        {data.license && <section className="panel-card setting-panel wide-setting">
          <div className="setting-icon"><KeyRound size={19} /></div>
          <div>
            <h3>{data.license.plan_name} plan</h3>
            <p>{data.license.edition !== "individual" ? "Paid self-hosted license. Bring your own AI; allowances follow your commercial agreement." : "Free forever for personal, noncommercial use. Professional work requires Freelancer, Teams or Enterprise."}</p>
            <div className="settings-line"><span>Users</span><b>{data.license.usage.users} / {data.license.limits.users ?? "Unlimited"}</b></div>
            <div className="settings-line"><span>Saved automations</span><b>{data.license.usage.automations} / {data.license.limits.automations ?? "Unlimited"}</b></div>
            <div className="settings-line"><span>License status</span><b>{data.license.license_status.replaceAll("_", " ")}</b></div>
            <div className="settings-line"><span>SSO entitlement</span><b>{data.license.features.sso ? "Included" : "Paid plans"}</b></div>
            {data.license.billing_period && <div className="settings-line"><span>Billing period</span><b>{data.license.billing_period}</b></div>}
            {data.license.customer && <div className="settings-line"><span>Licensed to</span><b>{data.license.customer}</b></div>}
            {data.license.expires_at && <div className="settings-line"><span>Expires</span><b>{date(new Date(data.license.expires_at * 1000).toISOString())}</b></div>}
            <div className="settings-line"><span>Installation ID</span><code>{data.license.installation_id}</code></div>
            {data.license.over_limit && <p role="alert">Existing data exceeds this plan's limits. It is retained for editing, deletion and export. Additional creations are blocked at the applicable limit.</p>}
            <p>Disabled automations count. To activate a paid plan, request a signed license for this installation ID, mount it through DIFFROOK_LICENSE_FILE and restart.</p>
            <a className="text-btn" href="https://github.com/Alexandre1116/diffrook/blob/main/docs/licensing.md" target="_blank" rel="noreferrer">License installation <ExternalLink size={14} /></a>
          </div>
        </section>}
        <section className="panel-card setting-panel">
          <div className="setting-icon">
            <ShieldCheck size={19} />
          </div>
          <div>
            <h3>Instance details</h3>
            <p>
              Diffrook runs on your server. No data is sent to a central
              service.
            </p>
            <div className="settings-line">
              <span>Version</span>
              <b>Self-hosted</b>
            </div>
            <div className="settings-line">
              <span>Automations</span>
              <b>{data.automations.length}</b>
            </div>
            <div className="settings-line">
              <span>Connections</span>
              <b>{data.connections.length}</b>
            </div>
            <div className="settings-line">
              <span>AI providers</span>
              <b>{data.providers.length}</b>
            </div>
          </div>
        </section>
        <section className="panel-card setting-panel">
          <div className="setting-icon amber">
            <Boxes size={19} />
          </div>
          <div>
            <h3>Export configuration</h3>
            <p>
              Download automations, connections, and provider settings as JSON.
              Secret values are omitted by the server.
            </p>
            <button className="btn secondary" onClick={exportConfig}>
              <ArrowDownRight size={15} />
              Download JSON
            </button>
          </div>
        </section>
        <section className="panel-card setting-panel wide-setting">
          <div className="setting-icon">
            <CircleHelp size={19} />
          </div>
          <div>
            <h3>Need to change server settings?</h3>
            <p>
              Authentication, encryption keys, storage, and webhook URLs are
              configured through your deployment environment.
            </p>
            <a
              className="text-btn"
              href="https://github.com/Alexandre1116/diffrook#run-with-docker"
              target="_blank"
              rel="noreferrer"
            >
              Deployment reference <ExternalLink size={14} />
            </a>
          </div>
        </section>
      </div>
      {data.catalog && data.license && <Plans catalog={data.catalog} edition={data.license.edition} installationId={data.license.installation_id} notice={notice} />}
    </>
  );
}

function Modal({
  title,
  close,
  wide,
  children,
}: {
  title: string;
  close: () => void;
  wide?: boolean;
  children: React.ReactNode;
}) {
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [close]);
  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        className={`modal ${wide ? "wide" : ""}`}
        role="dialog"
        aria-modal="true"
      >
        <div className="modal-header">
          <div>
            <div className="eyebrow">DIFFROOK CONFIGURATION</div>
            <h2>{title}</h2>
          </div>
          <button className="icon-btn" onClick={close}>
            <X size={19} />
          </button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}

function ResourceForm({
  kind,
  item,
  save,
  cancel,
}: {
  kind: "connection" | "provider" | "notification";
  item: Obj;
  save: (v: Obj) => void;
  cancel: () => void;
}) {
  const isConn = kind === "connection";
  const isNotif = kind === "notification";
  const [v, setV] = useState<Obj>({ ...item });
  const [err, setErr] = useState("");
  const [show, setShow] = useState(false);
  const change = (k: string, val: any) =>
    setV((x: Obj) => ({ ...x, [k]: val }));
  const choose = (k: string) => {
    change("kind", k);
    if (isNotif) return;
    if (isConn)
      change("base_url", k === "github" ? "https://api.github.com" : "");
    else
      change(
        "base_url",
        k === "openai"
          ? "https://api.openai.com/v1"
          : k === "anthropic"
            ? "https://api.anthropic.com"
            : "http://host.docker.internal:11434",
      );
  };
  return (
    <form
      className="form-stack"
      onSubmit={(e) => {
        e.preventDefault();
        if (!v.name.trim()) {
          setErr("Name is required");
          return;
        }
        save(v);
      }}
    >
      <div className="field-grid">
        <label>
          Name
          <input
            value={v.name || ""}
            onChange={(e) => change("name", e.target.value)}
            placeholder={
              isConn ? "Production GitHub" : isNotif ? "Engineering Discord" : "Primary model provider"
            }
            required
          />
        </label>
        <label>
          Type
          <select value={v.kind} onChange={(e) => choose(e.target.value)}>
            {(isConn
              ? ["github", "forgejo"]
              : isNotif ? ["discord", "slack", "teams", "webhook"] : ["openai", "ollama", "anthropic"]
            ).map((x) => (
              <option key={x} value={x}>
                {x === "github"
                  ? "GitHub"
                  : x === "forgejo"
                    ? "Forgejo"
                    : x === "openai"
                      ? "OpenAI"
                      : x === "ollama"
                      ? "Ollama"
                        : x === "discord"
                          ? "Discord"
                          : x === "slack"
                            ? "Slack"
                            : x === "teams"
                              ? "Microsoft Teams"
                              : x === "webhook"
                                ? "Generic webhook"
                                : "Anthropic"}
              </option>
            ))}
          </select>
        </label>
      </div>
      {isNotif ? (
        <label>
          Webhook URL
          <div className="password-wrap">
            <input
              type={show ? "text" : "password"}
              value={v.url || ""}
              onChange={(e) => change("url", e.target.value)}
              placeholder={item.has_url ? "Saved URL. Leave blank to keep it." : "https://..."}
              autoComplete="new-password"
              required={!item.has_url}
            />
            <button type="button" onClick={() => setShow(!show)} aria-label={show ? "Hide webhook URL" : "Show webhook URL"}>
              {show ? <EyeOff size={16} /> : <Eye size={16} />}
            </button>
          </div>
          <small>Webhook URLs contain secrets. Diffrook encrypts them at rest and never displays them after saving.</small>
        </label>
      ) : (
      <>
      <label>
        {isConn
          ? v.kind === "github"
            ? "API base URL"
            : "Forgejo instance URL"
          : "API base URL"}
        <input
          value={v.base_url || ""}
          onChange={(e) => change("base_url", e.target.value)}
          placeholder={
            isConn ? "https://api.github.com" : "https://api.openai.com/v1"
          }
          required
        />
        <small>
          {isConn
            ? "For Forgejo, use the instance root."
            : "Use a reachable URL from the Diffrook server."}
        </small>
      </label>
      <label>
        {isConn ? "Access token" : "API key"}
        <div className="password-wrap">
          <input
            value={isConn ? v.token || "" : v.api_key || ""}
            onChange={(e) =>
              change(isConn ? "token" : "api_key", e.target.value)
            }
            type={show ? "text" : "password"}
            placeholder={
              isConn
                ? item.has_token
                  ? "Saved token. Leave blank to keep it."
                  : "Paste an access token"
                : item.has_api_key
                  ? "Saved API key. Leave blank to keep it."
                  : "Paste an API key"
            }
            autoComplete="new-password"
          />
          <button type="button" onClick={() => setShow(!show)}>
            {show ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
        </div>
      </label>
      {isConn ? (
        <>
          <label>
            Webhook secret{" "}
            <input
              value={v.webhook_secret || ""}
              onChange={(e) => change("webhook_secret", e.target.value)}
              type="password"
              placeholder={
                item.has_webhook_secret
                  ? "Saved secret. Leave blank to keep it."
                  : "Optional signing secret"
              }
              autoComplete="new-password"
            />
            <small>
              Configure the same secret in your GitHub or Forgejo webhook.
            </small>
          </label>
          <label>
            Bot username{" "}
            <input
              value={v.bot_username || ""}
              onChange={(e) => change("bot_username", e.target.value)}
              placeholder="diffrook-bot"
            />
            <small>Used to ignore events created by this connection.</small>
          </label>
        </>
      ) : (
        <label>
          Default model
          <input
            value={v.default_model || ""}
            onChange={(e) => change("default_model", e.target.value)}
            placeholder={
              v.kind === "ollama"
                ? "qwen2.5-coder:14b"
                : v.kind === "anthropic"
                  ? "claude-sonnet-4-20250514"
                  : "gpt-4.1"
            }
            required
          />
        </label>
      )}
      {!isConn && v.kind === "openai" && (
        <div className="field-grid">
          <label>Output token parameter
            <select value={v.token_parameter || "max_tokens"} onChange={(e) => change("token_parameter", e.target.value)}>
              <option value="max_tokens">max_tokens</option>
              <option value="max_completion_tokens">max_completion_tokens</option>
            </select>
            <small>Use the parameter supported by your model endpoint.</small>
          </label>
          <label>JSON response mode
            <select value={v.json_mode ? "on" : "off"} onChange={(e) => change("json_mode", e.target.value === "on")}>
              <option value="off">Prompt instructions only</option>
              <option value="on">Request JSON mode</option>
            </select>
            <small>Enable only if your endpoint supports response_format.</small>
          </label>
        </div>
      )}
      </>
      )}
      {err && <div className="form-error">{err}</div>}
      <div className="form-actions">
        <button type="button" className="btn secondary" onClick={cancel}>
          Cancel
        </button>
        <button className="btn primary">
          <Check size={15} />
          Save {isConn ? "connection" : isNotif ? "destination" : "provider"}
        </button>
      </div>
    </form>
  );
}

function AutomationForm({
  item,
  connections,
  providers,
  notificationProviders,
  save,
  cancel,
}: {
  item: Obj;
  connections: Obj[];
  providers: Obj[];
  notificationProviders: Obj[];
  save: (v: Obj) => void;
  cancel: () => void;
}) {
  const [v, setV] = useState<Obj>(() => ({
    ...blankAutomation(),
    ...item,
    trigger: { ...emptyTrigger, ...item.trigger },
    filters: { ...blankAutomation().filters, ...item.filters },
    limits: { ...emptyLimits, ...item.limits },
    fix: { mode: "new_branch", branch_prefix: "diffrook/", ...item.fix },
    notifications: (item.notifications || []).map((n: Obj) => ({
      ...n,
      id: n.id || crypto.randomUUID(),
    })),
    repositories: [...(item.repositories || [])],
  }));
  const [err, setErr] = useState("");
  const [preview, setPreview] = useState<string[]>([]);
  const [previewBusy, setPreviewBusy] = useState(false);
  const upd = (k: string, val: any) => setV((x: Obj) => ({ ...x, [k]: val }));
  const nested = (key: string, k: string, val: any) =>
    setV((x: Obj) => ({ ...x, [key]: { ...x[key], [k]: val } }));
  const arr = (text: string) =>
    text
      .split(/\r?\n|,/)
      .map((x) => x.trim())
      .filter(Boolean);
  const textArr = (x: any) => (Array.isArray(x) ? x.join("\n") : "");
  const selected = (ev: string) => v.trigger.events?.includes(ev);
  const toggleEvent = (event: string, yes: boolean) =>
    nested(
      "trigger",
      "events",
      yes
        ? [...new Set([...(v.trigger.events || []), event])]
        : v.trigger.events.filter((e: string) => e !== event),
    );
  const addNotification = () => {
    const provider = notificationProviders[0];
    upd("notifications", [
      ...v.notifications,
      provider
        ? { id: crypto.randomUUID(), kind: provider.kind, provider_id: provider.id }
        : { id: crypto.randomUUID(), kind: "pr_comment", url: "" },
    ]);
  };
  const chooseNotification = (i: number, value: string) => {
    if (value.startsWith("legacy:")) return;
    const items = [...v.notifications];
    const current = { ...items[i] };
    if (value.startsWith("provider:")) {
      const provider = notificationProviders.find((p) => p.id === value.slice(9));
      if (!provider) return;
      items[i] = { ...current, kind: provider.kind, provider_id: provider.id, url: "", has_url: false };
    } else {
      items[i] = { ...current, kind: value, provider_id: undefined, url: "", has_url: false };
    }
    upd("notifications", items);
  };
  const removeNotification = (i: number) =>
    upd(
      "notifications",
      v.notifications.filter((_: any, j: number) => j !== i),
    );
  const getPreview = async () => {
    setPreviewBusy(true);
    setErr("");
    try {
      const r = await api<{ next: string[] }>("/schedule/preview", "POST", {
        cron: v.trigger.cron,
        timezone: v.trigger.timezone,
      });
      setPreview(r.next || []);
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setPreviewBusy(false);
    }
  };
  const validate = (e: React.FormEvent) => {
    e.preventDefault();
    if (!v.name.trim()) {
      setErr("Give this automation a name.");
      return;
    }
    if (!v.connection_id || !v.provider_id) {
      setErr("Choose a connection and an AI provider.");
      return;
    }
    if (v.repositories.some((r: string) => !/^[-\w.]+\/[-\w.]+$/.test(r))) {
      setErr("Repositories must use owner/repo format.");
      return;
    }
    if (!v.repositories.length) {
      setErr("Add at least one repository.");
      return;
    }
    if (
      v.trigger.schedule_enabled &&
      (!v.trigger.cron || !v.trigger.timezone)
    ) {
      setErr("Schedule needs both a cron expression and a timezone.");
      return;
    }
    save(v);
  };
  return (
    <form className="automation-form" onSubmit={validate}>
      <div className="form-scroll">
        <section className="editor-section">
          <div className="editor-section-head">
            <span className="section-index">01</span>
            <div>
              <h3>Basics</h3>
              <p>Name this workflow and choose the code host and model.</p>
            </div>
          </div>
          <div className="form-grid">
            <label>
              Automation name
              <input
                value={v.name}
                onChange={(e) => upd("name", e.target.value)}
                placeholder="PR review on core services"
                required
              />
            </label>
            <label>
              Action
              <select
                value={v.action}
                onChange={(e) => {
                  const action = e.target.value;
                  setV((old: Obj) => ({...old, action,
                    trigger: {...old.trigger,
                      events: action === "solve_issue" ? ["issues.labeled"] : action === "audit" ? [] : ["pull_request.opened", "pull_request.synchronize"],
                      schedule_target: action === "solve_issue" ? "open_issues" : action === "audit" ? "repository" : "open_pull_requests"},
                    fix: {...old.fix, mode: action === "solve_issue" ? "new_branch" : old.fix.mode}
                  }));
                }}
              >
                <option value="review">Review a pull request</option>
                <option value="fix_pr">
                  Fix a pull request
                </option>
                <option value="solve_issue">Solve issues</option>
                <option value="audit">Audit a repository branch</option>
              </select>
            </label>
            <label className="span-2">
              Description{" "}
              <input
                value={v.description}
                onChange={(e) => upd("description", e.target.value)}
                placeholder="What should this automation handle?"
              />
            </label>
            <label>
              Code host connection
              <select
                value={v.connection_id}
                onChange={(e) => upd("connection_id", e.target.value)}
                required
              >
                <option value="">Choose a connection</option>
                {connections.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name} · {c.kind}
                  </option>
                ))}
              </select>
            </label>
            <label>
              AI provider
              <select
                value={v.provider_id}
                onChange={(e) => upd("provider_id", e.target.value)}
                required
              >
                <option value="">Choose a provider</option>
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name} · {p.kind}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Model override{" "}
              <input
                value={v.model || ""}
                onChange={(e) => upd("model", e.target.value)}
                placeholder="Use provider default"
              />
              <small>
                Leave blank to use the selected provider's default model.
              </small>
            </label>
            <label className="switch-field">
              <span>
                <b>Automation enabled</b>
                <small>
                  Paused automations do not respond to events or schedules.
                </small>
              </span>
              <Switch checked={v.enabled} onChange={(x) => upd("enabled", x)} />
            </label>
          </div>
          <div className="field-callout">
            <ShieldCheck size={16} />
            <span>
              Reviews publish comments only when a comment channel is selected.
              Fix actions can write code. Diffrook never merges a pull request.
            </span>
          </div>
        </section>
        <section className="editor-section">
          <div className="editor-section-head">
            <span className="section-index">02</span>
            <div>
              <h3>Repository targets</h3>
              <p>
                Use one <code>owner/repo</code> per line.
              </p>
            </div>
          </div>
          <label>
            Repositories
            <textarea
              rows={3}
              value={v.repositories.join("\n")}
              onChange={(e) => upd("repositories", arr(e.target.value))}
              placeholder={"acme/api\nacme/web"}
            />
            <small>Diffrook will only act on these repositories.</small>
          </label>
        </section>
        <section className="editor-section">
          <div className="editor-section-head">
            <span className="section-index">03</span>
            <div>
              <h3>Triggers and schedule</h3>
              <p>
                Choose repository events, command mentions, or a recurring run.
              </p>
            </div>
          </div>
          <div className="event-grid">
            {events.map((ev) => (
              <label className="check-card" key={ev}>
                <input
                  type="checkbox"
                  checked={selected(ev)}
                  onChange={(e) => toggleEvent(ev, e.target.checked)}
                />
                <span className="check-mark">
                  <Check size={13} />
                </span>
                <span>
                  <b>
                    {ev.includes("pull_request")
                      ? "Pull request"
                      : ev.includes("issues")
                        ? "Issue"
                        : "Comment command"}
                  </b>
                  <small>{ev}</small>
                </span>
              </label>
            ))}
          </div>
          <label className="command-field">
            Command mention
            <input
              value={v.trigger.command || ""}
              onChange={(e) => nested("trigger", "command", e.target.value)}
              placeholder="/diffrook"
            />
            <small>Used when issue_comment.command is enabled.</small>
          </label>
          <div className="switch-card">
            <span className="switch-icon">
              <CalendarClock size={17} />
            </span>
            <span>
              <b>Recurring schedule</b>
              <small>
                Run on open pull requests, open issues, or a repository branch.
              </small>
            </span>
            <Switch
              checked={v.trigger.schedule_enabled}
              onChange={(x) => nested("trigger", "schedule_enabled", x)}
            />
          </div>
          {v.trigger.schedule_enabled && (
            <div className="schedule-box">
              <div className="form-grid">
                <label>
                  Cron expression
                  <input
                    value={v.trigger.cron}
                    onChange={(e) => nested("trigger", "cron", e.target.value)}
                    placeholder="0 9 * * 1"
                  />
                  <small>
                    Five fields, for example 0 9 * * 1 for Mondays at 09:00.
                  </small>
                </label>
                <label>
                  Timezone
                  <input
                    value={v.trigger.timezone}
                    onChange={(e) =>
                      nested("trigger", "timezone", e.target.value)
                    }
                    placeholder="Europe/Lisbon"
                  />
                  <small>Use an IANA timezone such as Europe/Lisbon.</small>
                </label>
                <label>
                  Scheduled target
                  <select
                    value={v.trigger.schedule_target}
                    onChange={(e) =>
                      nested("trigger", "schedule_target", e.target.value)
                    }
                  >
                    <option value="open_pull_requests">
                      Open pull requests
                    </option>
                    <option value="open_issues">Open issues</option>
                    <option value="repository">Repository branch audit</option>
                  </select>
                </label>
                {v.trigger.schedule_target === "repository" && (
                  <label>
                    Branch
                    <input
                      value={v.trigger.branch || "main"}
                      onChange={(e) =>
                        nested("trigger", "branch", e.target.value)
                      }
                      placeholder="main"
                    />
                  </label>
                )}
              </div>
              <div className="preview-row">
                <button
                  type="button"
                  className="btn secondary small"
                  onClick={getPreview}
                  disabled={previewBusy}
                >
                  {previewBusy ? (
                    <LoaderCircle size={14} className="spin" />
                  ) : (
                    <Clock3 size={14} />
                  )}
                  Preview next runs
                </button>
                {preview.length > 0 && (
                  <span className="preview-dates">
                    {preview.slice(0, 3).map((d) => (
                      <i key={d}>{date(d)}</i>
                    ))}
                  </span>
                )}
              </div>
            </div>
          )}
        </section>
        <section className="editor-section">
          <div className="editor-section-head">
            <span className="section-index">04</span>
            <div>
              <h3>Filters</h3>
              <p>Skip drafts, limit actors, labels, and paths.</p>
            </div>
          </div>
          <div className="form-grid">
            <label>
              Required labels
              <textarea
                rows={2}
                value={textArr(v.filters.labels)}
                onChange={(e) =>
                  nested("filters", "labels", arr(e.target.value))
                }
                placeholder="needs-review"
              />
              <small>
                Only items with at least one matching label are included.
              </small>
            </label>
            <label>
              Allowed actors
              <textarea
                rows={2}
                value={textArr(v.filters.allowed_actors)}
                onChange={(e) =>
                  nested("filters", "allowed_actors", arr(e.target.value))
                }
                placeholder="octocat"
              />
              <small>
                Only actors allowed by the repository integration can trigger
                commands.
              </small>
            </label>
            <label className="span-2">
              Ignored paths
              <textarea
                rows={2}
                value={textArr(v.filters.ignore_paths)}
                onChange={(e) =>
                  nested("filters", "ignore_paths", arr(e.target.value))
                }
              />
              <small>
                Glob patterns, one per line. Defaults include vendor, dist, and
                lock files.
              </small>
            </label>
            <label className="switch-field">
              <span>
                <b>Ignore draft pull requests</b>
                <small>Skip pull requests until marked ready for review.</small>
              </span>
              <Switch
                checked={v.filters.ignore_drafts !== false}
                onChange={(x) => nested("filters", "ignore_drafts", x)}
              />
            </label>
          </div>
        </section>
        <section className="editor-section">
          <div className="editor-section-head">
            <span className="section-index">05</span>
            <div>
              <h3>Instructions and limits</h3>
              <p>Tell the model what matters and cap its work per run.</p>
            </div>
          </div>
          <label>
            Instructions
            <textarea
              rows={5}
              value={v.instructions}
              onChange={(e) => upd("instructions", e.target.value)}
              placeholder="Focus on correctness, security, and user-visible regressions. Explain each finding with a file and line reference."
            />
            <small>
              Repository content is treated as untrusted input and cannot change
              these instructions.
            </small>
          </label>
          <div className="number-grid">
            {Object.entries(v.limits).map(([k, val]) => (
              <label key={k}>
                {limitLabel(k)}
                <input
                  type="number"
                  min="1"
                  value={val as number}
                  onChange={(e) => nested("limits", k, Number(e.target.value))}
                />
              </label>
            ))}
          </div>
        </section>
        {["fix_pr", "solve_issue"].includes(v.action) && (
          <section className="editor-section">
            <div className="editor-section-head">
              <span className="section-index">06</span>
              <div>
                <h3>Fix destination</h3>
                <p>Choose where structured fixes should be published.</p>
              </div>
            </div>
            <div className="form-grid">
              <label>
                Branch mode
                <select
                  value={v.fix.mode}
                  onChange={(e) => nested("fix", "mode", e.target.value)}
                >
                  <option value="new_branch">Create a new branch</option>
                  {v.action === "fix_pr" && <option value="existing_branch">
                    Use an existing branch
                  </option>}
                </select>
              </label>
              <label>
                Branch prefix
                <input
                  value={v.fix.branch_prefix}
                  onChange={(e) =>
                    nested("fix", "branch_prefix", e.target.value)
                  }
                  placeholder="diffrook/"
                />
              </label>
            </div>
          </section>
        )}
        <section className="editor-section">
          <div className="editor-section-head">
            <span className="section-index">
              {["fix_pr", "solve_issue"].includes(v.action) ? "07" : "06"}
            </span>
            <div>
              <h3>Notifications</h3>
              <p>Send a message when a run finishes. Manage reusable destinations in Notifications.</p>
            </div>
            <button
              type="button"
              className="btn secondary small"
              onClick={addNotification}
            >
              <Plus size={14} />
              Add channel
            </button>
          </div>
          {v.notifications.length ? (
            <div className="notification-list">
              {v.notifications.map((n: Obj, i: number) => (
                <div className="notification-row" key={n.id || i}>
                  <select
                    value={n.provider_id ? `provider:${n.provider_id}` : n.has_url && ["discord", "slack", "teams", "webhook"].includes(n.kind) ? `legacy:${i}` : n.kind}
                    onChange={(e) => chooseNotification(i, e.target.value)}
                  >
                    <option value="pr_comment">Pull request comment</option>
                    <option value="issue_comment">Issue comment</option>
                    {n.has_url && ["discord", "slack", "teams", "webhook"].includes(n.kind) && (
                      <option value={`legacy:${i}`}>Legacy {n.kind} URL</option>
                    )}
                    <optgroup label="Saved notification providers">
                      {notificationProviders.map((p) => (
                        <option key={p.id} value={`provider:${p.id}`}>{p.name} · {p.kind}</option>
                      ))}
                      {!notificationProviders.length && <option disabled value="">Add a destination in Notifications first</option>}
                    </optgroup>
                  </select>
                  <span className="notification-hint">
                    {["pr_comment", "issue_comment"].includes(n.kind)
                      ? "Posts through the selected code host"
                      : n.provider_id
                        ? notificationProviders.find((p) => p.id === n.provider_id)?.has_url ? "Saved destination · URL encrypted" : "Select a saved destination"
                        : n.has_url ? "Legacy per-automation URL · retained on save" : "Select a saved destination"}
                  </span>
                  <button
                    type="button"
                    className="icon-btn danger-icon"
                    onClick={() => removeNotification(i)}
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              ))}
            </div>
          ) : (
            <div className="inline-empty">No notification channels added.</div>
          )}
        </section>
      </div>
      {err && (
        <div className="form-error sticky-error">
          <TriangleAlert size={15} />
          {err}
        </div>
      )}
      <div className="form-actions sticky-actions">
        <button type="button" className="btn secondary" onClick={cancel}>
          Cancel
        </button>
        <button className="btn primary">
          <Check size={15} />
          Save automation
        </button>
      </div>
    </form>
  );
}
function limitLabel(k: string) {
  return (
    (
      {
        max_files: "Maximum files",
        max_file_bytes: "Maximum bytes per file",
        max_context_chars: "Maximum context characters",
        max_output_tokens: "Maximum output tokens",
        timeout_seconds: "Timeout in seconds",
        max_fix_files: "Maximum files to fix",
      } as Record<string, string>
    )[k] || k
  );
}
function Switch({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (b: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      className={`switch ${checked ? "on" : ""}`}
      onClick={() => onChange(!checked)}
    >
      <span />
    </button>
  );
}

function RunForm({
  item,
  submit,
  cancel,
}: {
  item: Obj;
  submit: (v: Obj) => void;
  cancel: () => void;
}) {
  const [kind, setKind] = useState(item.action === "solve_issue" ? "issue" : item.action === "audit" ? "repository" : "pull_request");
  const [repo, setRepo] = useState(item.repositories?.[0] || "");
  const [number, setNumber] = useState("");
  const [branch, setBranch] = useState("main");
  const repos = item.repositories || [];
  const label = actionLabel(item.action);
  return (
    <form
      className="form-stack"
      onSubmit={(e) => {
        e.preventDefault();
        if (!repo) return;
        submit({
          kind,
          repository: repo,
          ...(kind !== "repository" ? { number: Number(number) } : { branch }),
        });
      }}
    >
      <div className="run-modal-banner">
        <Play size={17} />
        <span>
          <b>{item.name}</b>
          <small>{label}</small>
        </span>
      </div>
      <label>
        Repository
        <select required value={repo} onChange={(e) => setRepo(e.target.value)}>
          <option value="">Choose a repository</option>
          {repos.map((r: string) => (
            <option key={r}>{r}</option>
          ))}
        </select>
      </label>
      <div className="target-choice">
        <span>Run target</span>
        {[
          { id: "pull_request", label: "Pull request", icon: GitPullRequest },
          { id: "issue", label: "Issue", icon: CircleHelp },
          { id: "repository", label: "Repository", icon: Code2 },
        ].filter(t => t.id === (item.action === "solve_issue" ? "issue" : item.action === "audit" ? "repository" : "pull_request")).map((t) => (
          <button
            type="button"
            key={t.id}
            className={kind === t.id ? "active" : ""}
            onClick={() => setKind(t.id)}
          >
            <t.icon size={16} />
            {t.label}
          </button>
        ))}
      </div>
      {kind === "repository" ? (
        <label>
          Branch
          <input
            required
            value={branch}
            onChange={(e) => setBranch(e.target.value)}
            placeholder="main"
          />
        </label>
      ) : (
        <label>
          {kind === "issue" ? "Issue number" : "Pull request number"}
          <input
            type="number"
            min="1"
            value={number}
            onChange={(e) => setNumber(e.target.value)}
            placeholder="42"
            required
          />
        </label>
      )}
      <div className="field-callout">
        <ShieldCheck size={15} />
        <span>
          Diffrook will use this automation's connection, model, filters, and
          limits.
        </span>
      </div>
      <div className="form-actions">
        <button type="button" className="btn secondary" onClick={cancel}>
          Cancel
        </button>
        <button className="btn primary">
          <Play size={14} />
          Queue run
        </button>
      </div>
    </form>
  );
}

function RunDetail({ run, close }: { run: Obj; close: () => void }) {
  const output = run.output || {};
  const findings = output.findings || [];
  const usage = output.usage || {};
  return (
    <Modal title="Run details" close={close} wide>
      <div className="detail-overview">
        <div>
          <div className="eyebrow">{run.automation_name || "AUTOMATION"}</div>
          <h3>
            {run.trigger?.repository || "Run"}
            {run.trigger?.number ? ` #${run.trigger.number}` : ""}
          </h3>
          <p>
            Created {date(run.created_at)} · Started {date(run.started_at)} ·
            Finished {date(run.finished_at)}
          </p>
        </div>
        <span className={statusClass(run.status)}>
          <i />
          {run.status}
        </span>
      </div>
      {run.error && (
        <div className="detail-error">
          <TriangleAlert size={17} />
          <span>
            <b>Run error</b>
            {run.error}
          </span>
        </div>
      )}
      <div className="detail-stats">
        <div>
          <small>FINDINGS</small>
          <b>{findings.length}</b>
        </div>
        <div>
          <small>FILES REVIEWED</small>
          <b>
            {output.artifacts?.included_files?.length ?? "Unavailable"}
          </b>
        </div>
        <div>
          <small>MODEL</small>
          <b>{usage.model || output.model || "—"}</b>
        </div>
        <div>
          <small>TOKENS</small>
          <b>
            {usage.total_tokens ??
              (usage.input_tokens != null || usage.output_tokens != null
                ? (usage.input_tokens || 0) + (usage.output_tokens || 0)
                : "Unavailable")}
          </b>
        </div>
      </div>
      <section className="detail-section">
        <h4>
          <TriangleAlert size={16} />
          Findings <span>{findings.length}</span>
        </h4>
        {findings.length ? (
          <div className="findings">
            {findings.map((f: Obj, i: number) => (
              <article key={i}>
                <div className="finding-top">
                  <span
                    className={`severity severity-${(f.severity || "info").toLowerCase()}`}
                  >
                    {f.severity || "Finding"}
                  </span>
                  <span>
                    {f.file || f.path || ""}
                    {f.line ? `:${f.line}` : ""}
                  </span>
                </div>
                <b>{f.title || f.summary || "Finding"}</b>
                <p>
                  {f.explanation || f.description || f.body || f.message || ""}
                </p>
                {f.suggestion && <p><strong>Suggested fix:</strong> {f.suggestion}</p>}
              </article>
            ))}
          </div>
        ) : (
          <p className="empty-inline">
            No findings were recorded for this run.
          </p>
        )}
      </section>
      <section className="detail-section">
        <h4>
          <Boxes size={16} />
          Coverage
        </h4>
        <pre className="json-box">
          {JSON.stringify(
            output.artifacts?.coverage ||
              output.artifacts || {
                summary: output.summary || "No coverage details returned.",
              },
            null,
            2,
          )}
        </pre>
      </section>
      <section className="detail-section">
        <h4>
          <Terminal size={16} />
          Logs
        </h4>
        <pre className="logs-box">
          {Array.isArray(output.logs)
            ? output.logs.join("\n")
            : typeof output.logs === "string"
              ? output.logs
              : JSON.stringify(output.logs || "No logs returned.", null, 2)}
        </pre>
      </section>
      {output.artifacts && (
        <section className="detail-section">
          <h4>Artifacts</h4>
          <pre className="json-box">
            {JSON.stringify(output.artifacts, null, 2)}
          </pre>
        </section>
      )}
    </Modal>
  );
}

export default App;
