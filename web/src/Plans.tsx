import { useState } from "react";
import { Check, ExternalLink } from "lucide-react";
import "./plans.css";

type Plan = {
  id: string; name: string; users: number | null; automations: number | null;
  min_users?: number; automations_per_user?: number; sso: boolean;
  commercial_use: boolean; monthly_eur: number; annual_eur: number;
  per_user: boolean; free_forever?: boolean;
  executions_per_month?: number; executions_per_user_per_month?: number;
};
export type PlanCatalog = {
  currency: string; self_hosted: Plan[];
  self_hosted_automation_pack: { automations: number; monthly_eur: number; annual_eur: number };
  cloud: {
    status: string; purchase_available: boolean; plans: Plan[];
    automation_pack: { automations: number; monthly_eur: number; annual_eur: number };
    execution_pack: { executions: number; monthly_eur: number };
    ai: { token_unit: number; models: { id: string; name: string; input_eur: number; output_eur: number }[] };
  };
};
const money = (amount: number) => new Intl.NumberFormat("en-IE", { style: "currency", currency: "EUR", maximumFractionDigits: 2 }).format(amount);
const instructions = "https://github.com/Alexandre1116/diffrook/blob/main/docs/licensing.md";

export default function Plans({ catalog, edition, installationId, notice }: {
  catalog: PlanCatalog; edition: string; installationId: string; notice: (text: string) => void;
}) {
  const [hosting, setHosting] = useState<"self_hosted" | "cloud">("self_hosted");
  const [period, setPeriod] = useState<"monthly" | "annual">("monthly");
  const [seats, setSeats] = useState(String(catalog.self_hosted.find(p => p.per_user)!.min_users)), [packs, setPacks] = useState("0");
  const cloud = hosting === "cloud";
  const plans = cloud ? catalog.cloud.plans : catalog.self_hosted;
  const pack = cloud ? catalog.cloud.automation_pack : catalog.self_hosted_automation_pack;
  const enterprisePlan = plans.find(p => p.per_user)!;
  const users = Number(seats), extraPacks = Number(packs);
  const validEnterprise = seats.trim() !== "" && packs.trim() !== "" && Number.isSafeInteger(users) && users >= enterprisePlan.min_users!
    && Number.isSafeInteger(extraPacks) && extraPacks >= 0 && users * enterprisePlan.automations_per_user! + extraPacks * pack.automations <= 2147483647;
  const prepareRequest = (plan: Plan) => {
    if (cloud || plan.free_forever || (plan.per_user && !validEnterprise)) return;
    const count = plan.per_user ? users : plan.users;
    const extra = plan.per_user ? extraPacks : 0;
    const request = {
      request_version: 1, hosting: "self_hosted", installation_id: installationId,
      plan: plan.id, billing_period: period, users: count, extra_automation_packs: extra,
      automations: plan.per_user ? users * plan.automations_per_user! + extra * pack.automations : plan.automations,
      currency: catalog.currency, tax_included: false,
      quoted_amount: plan[`${period}_eur`] * (plan.per_user ? users : 1) + extra * pack[`${period}_eur`],
    };
    const url = URL.createObjectURL(new Blob([JSON.stringify(request, null, 2)], { type: "application/json" }));
    const link = document.createElement("a"); link.href = url; link.download = `diffrook-${plan.id}-license-request.json`; link.click(); URL.revokeObjectURL(url);
    notice("License request downloaded. Arrange payment with the Diffrook owner and obtain a signed license to activate it.");
  };
  return <section className="plan-catalog" aria-label="Plans and pricing">
    <div className="plan-catalog-head">
      <div><h2>Plans and pricing</h2><p>Choose the allowance for your installation. Prices exclude applicable tax.</p></div>
      <div className="plan-switches">
        <div className="plan-toggle" role="group" aria-label="Hosting">
          <button type="button" aria-pressed={!cloud} onClick={() => setHosting("self_hosted")}>Self-hosted</button>
          <button type="button" aria-pressed={cloud} onClick={() => setHosting("cloud")}>Cloud <span>Coming soon</span></button>
        </div>
        <div className="plan-toggle" role="group" aria-label="Billing period">
          <button type="button" aria-pressed={period === "monthly"} onClick={() => setPeriod("monthly")}>Monthly</button>
          <button type="button" aria-pressed={period === "annual"} onClick={() => setPeriod("annual")}>Annual <span>2 months free</span></button>
        </div>
      </div>
    </div>
    <p className="plan-explainer">{cloud
      ? "Coming soon. Preview prices for hosting, backups and managed updates. AI usage is billed separately. Purchases and activation are unavailable."
      : "Run Diffrook on your server with your own AI provider or local model. Paid licenses renew monthly or annually; executions have no license quota."}</p>
    <div className="plan-grid">{plans.map(plan => {
      const enterprise = plan.per_user;
      const amount = plan[`${period}_eur`];
      const total = amount * (enterprise ? users : 1) + (enterprise ? extraPacks * pack[`${period}_eur`] : 0);
      const automations = enterprise ? users * plan.automations_per_user! + extraPacks * pack.automations : plan.automations;
      const current = !cloud && edition === plan.id;
      return <article className={`panel-card plan-card${current ? " current-plan" : ""}`} key={plan.id}>
        <div className="plan-card-heading"><h3>{plan.name}</h3>{current && <span className="plan-badge">Current plan</span>}{cloud && <span className="plan-badge">Coming soon</span>}</div>
        <div className="plan-price">{plan.free_forever ? "Free" : money(amount)}{!plan.free_forever && <small>/{enterprise ? "user/" : ""}{period === "annual" ? "year" : "month"}</small>}</div>
        <p className="plan-use">{plan.free_forever ? "Free forever for personal use" : plan.commercial_use ? "Professional and commercial use" : "Personal, noncommercial use"}</p>
        <ul className="plan-features">
          <li><Check size={14} />{enterprise ? `From ${plan.min_users} users` : `${plan.users} ${plan.users === 1 ? "user" : "users"}`}</li>
          <li><Check size={14} />{enterprise ? `${plan.automations_per_user} automations per user, pooled` : `${plan.automations} saved automations`}</li>
          <li><Check size={14} />{plan.sso ? "SSO with OpenID Connect" : "Local account sign-in"}</li>
          <li><Check size={14} />{cloud ? "Your AI key or pay as you go" : "Bring your own AI"}</li>
          {cloud && <li><Check size={14} />{enterprise ? `${plan.executions_per_user_per_month!.toLocaleString()} executions/user/month` : `${plan.executions_per_month!.toLocaleString()} executions/month`}</li>}
        </ul>
        {enterprise && <div className="plan-enterprise">
          <label>Licensed users<input aria-label={`${cloud ? "Cloud" : "Self-hosted"} Enterprise users`} type="number" min={plan.min_users} step={1} value={seats} onChange={e => setSeats(e.target.value)} /></label>
          <label>Extra packs of {pack.automations} automations<input aria-label={`${cloud ? "Cloud" : "Self-hosted"} Enterprise automation packs`} type="number" min={0} step={1} value={packs} onChange={e => setPacks(e.target.value)} /></label>
          <p>{money(pack[`${period}_eur`])} per pack/{period === "annual" ? "year" : "month"}</p>
          {validEnterprise ? <p className="plan-total">{money(total)}/{period === "annual" ? "year" : "month"} · {automations} automations</p> : <p role="alert">Enter at least {plan.min_users} users and a valid pack count.</p>}
        </div>}
        <div className="plan-card-action">{cloud
          ? <button className="btn secondary full" type="button" disabled>Coming soon</button>
          : plan.free_forever ? <span className="plan-free-note">No license fee · Personal use only</span>
          : <button className="btn secondary full" type="button" disabled={enterprise && !validEnterprise} onClick={() => prepareRequest(plan)}>Prepare license request</button>}
        </div>
      </article>;
    })}</div>
    {cloud ? <div className="panel-card plan-ai-preview">
      <h3>Managed AI · Coming soon</h3>
      <p>Use your own key and pay your provider directly, or pay Diffrook for the tokens consumed each month. The annual subscription discount does not apply to AI usage.</p>
      <div className="plan-table-wrap"><table><caption>Proposed rates per {catalog.cloud.ai.token_unit.toLocaleString()} tokens</caption><thead><tr><th>Model</th><th>Input</th><th>Output</th></tr></thead><tbody>{catalog.cloud.ai.models.map(model => <tr key={model.id}><th scope="row">{model.name}</th><td>{money(model.input_eur)}</td><td>{money(model.output_eur)}</td></tr>)}</tbody></table></div>
      <p>Cache will have separate rates. Planned controls include a monthly AI budget, alerts and a stop at the budget limit.</p>
      <p>Additional {catalog.cloud.execution_pack.executions.toLocaleString()} executions: {money(catalog.cloud.execution_pack.monthly_eur)}, with your approval, plus AI usage. Cloud limits will be validated before launch. Dedicated hosting and availability commitments require a separate quote.</p>
    </div> : <p className="plan-explainer">Download a request for the Diffrook owner. Payment and renewal are arranged directly; the request does not activate a plan. Install the signed license for this installation ID. <a className="text-btn" href={instructions} target="_blank" rel="noreferrer">License instructions <ExternalLink size={13} /></a></p>}
  </section>;
}
