<script>
  import { createEventDispatcher, onMount } from "svelte";
  import { t } from "./i18n.js";
  import { currentNetwork } from "./network.js";

  const dispatch = createEventDispatcher();

  // Live clock for countdown timers
  let now = new Date();
  let clockInterval;
  onMount(() => {
    clockInterval = setInterval(() => { now = new Date(); }, 1000);
    return () => clearInterval(clockInterval);
  });

  // Parse a 6-field CRON expression to period duration in ms.
  // Supports every-N-minutes, hourly, daily, weekly, monthly.
  function parsePeriodMs(cron) {
    if (!cron) return null;
    const parts = String(cron).trim().split(/\s+/);
    let minF, hourF, domF;
    if (parts.length === 6) [, minF, hourF, domF] = parts;
    else if (parts.length === 5) [minF, hourF, domF] = parts;
    else return null;

    if (minF && minF.startsWith('*/')) return parseInt(minF.slice(2)) * 60 * 1000;
    if (domF === '*' && hourF === '*') return 60 * 60 * 1000; // hourly
    if (domF === '*') return 24 * 60 * 60 * 1000;             // daily
    return 30 * 24 * 60 * 60 * 1000;                          // monthly fallback
  }

  /** Return remaining ms until next_payment_at (capped at periodMs) */
  function remainingMs(sub, now) {
    if (!sub.next_payment_at) return null;
    const next = new Date(sub.next_payment_at);
    return Math.max(0, next - now);
  }

  /** Human-readable countdown */
  function countdownText(sub, now) {
    const ms = remainingMs(sub, now);
    if (ms === null) return null;
    if (ms <= 0) return "Maintenant";
    const s = Math.floor(ms / 1000);
    if (s < 60) return `Dans ${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `Dans ${m}min`;
    const h = Math.floor(m / 60);
    if (h < 24) return `Dans ${h}h`;
    const d = Math.floor(h / 24);
    return `J-${d}`;
  }

  /** Fraction of period elapsed (0 = just paid, 1 = due now), for pie chart */
  function pieFraction(sub, now) {
    const rem = remainingMs(sub, now);
    const period = parsePeriodMs(sub.raw_schedule || sub.schedule);
    if (rem === null || !period) return 0;
    return Math.min(1, Math.max(0, 1 - rem / period));
  }

  /** SVG pie arc path for a fraction 0–1 */
  function piePath(f) {
    const r = 10, cx = 12, cy = 12;
    if (f >= 1) return `M${cx},${cy} m-${r},0 a${r},${r} 0 1 1 ${2*r},0 a${r},${r} 0 1 1 -${2*r},0`;
    const angle = f * 2 * Math.PI;
    const x = cx + r * Math.sin(angle);
    const y = cy - r * Math.cos(angle);
    const large = f > 0.5 ? 1 : 0;
    return `M${cx},${cy} L${cx},${cy - r} A${r},${r} 0 ${large} 1 ${x},${y} Z`;
  }

  // Kebab menu state
  let openMenuId = null;

  function toggleMenu(id, event) {
    event.stopPropagation();
    openMenuId = openMenuId === id ? null : id;
  }

  function closeMenu() { openMenuId = null; }

  function deleteSub(sub, event) {
    event.stopPropagation();
    if (confirm(`Supprimer « ${sub.label} » ?`)) {
      dispatch("delete-sub", { id: sub.id });
    }
    openMenuId = null;
  }

  function togglePause(sub, event) {
    event.stopPropagation();
    dispatch("toggle-pause", { id: sub.id });
    openMenuId = null;
  }

  function triggerNow(sub, event) {
    event.stopPropagation();
    dispatch("trigger-now", { sub });
    openMenuId = null;
  }

  function editSub(sub, event) {
    event.stopPropagation();
    dispatch("edit-sub", { sub });
    openMenuId = null;
  }

  export let archived = false;
  export let wallets = [];
  // All subscriptions passed from parent (Dashboard manages the list)
  export let subscriptions = [];

  const SUSPENDED_STATUSES = ["Suspended", "Cancelled", "Expired"];

  $: subs = subscriptions.filter(s =>
    archived
      ? ["Cancelled", "Expired"].includes(s.status)
      : !["Cancelled", "Expired"].includes(s.status)
  );

  function walletName(id) {
    const w = wallets.find(w => w.id === id);
    return w ? w.name : null;
  }
  function walletColor(id) {
    const w = wallets.find(w => w.id === id);
    return w ? w.color : "var(--text-muted)";
  }

  function statusDotColor(sub) {
    if (["Expired", "AddressMismatch", "SignatureInvalid"].includes(sub.canary_status)) return "red";
    if (sub.canary_status === "Unreachable") return "orange";
    if (["Paused", "PastDue"].includes(sub.status)) return "orange";
    if (sub.status === "Active") return "green";
    return "grey";
  }

  function statusBadgeClass(sub) {
    if (sub.status === "Active") return "badge-green";
    if (["Paused", "PastDue"].includes(sub.status)) return "badge-orange";
    if (["Suspended", "Expired", "Cancelled"].includes(sub.status)) return "badge-red";
    return "badge-grey";
  }

  function statusLabel(sub, t) {
    if (sub.status === "Suspended" && sub.canary_status === "Expired") return t.canaryExpired;
    if (sub.status === "Suspended") return t.suspended;
    if (sub.status === "Active") return t.active;
    if (sub.status === "Paused") return t.paused;
    if (sub.status === "Cancelled") return t.cancelled;
    if (sub.status === "Expired") return t.expired;
    if (sub.status === "Pending") return t.pending;
    return sub.status;
  }

  function isSuspended(sub) {
    return SUSPENDED_STATUSES.includes(sub.status);
  }

  function formatDate(dateStr) {
    if (!dateStr) return null;
    return new Date(dateStr).toLocaleDateString(undefined, { day: "2-digit", month: "short" });
  }
</script>

<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
<svelte:window on:click={closeMenu} />

<div class="sub-list" class:archived-list={archived}>
  {#if subs.length === 0}
    <div class="empty-state card">
      <p>{$t.noSubscriptions}</p>
      <p class="text-muted">{$t.noSubscriptionsHint}</p>
    </div>
  {:else}
    {#each subs as sub (sub.id)}
      <div class="sub-card card" class:sub-archived={archived} class:sub-suspended={isSuspended(sub)}>
        <div class="sub-left">
          <span class="status-dot {statusDotColor(sub)}"></span>
        </div>

        <div class="sub-body">
          <div class="sub-header">
            <span class="sub-label">{sub.label}</span>
            <span class="badge {statusBadgeClass(sub)}">{statusLabel(sub, $t)}</span>
          </div>

          {#if sub.description}
            <div class="sub-desc text-muted">{sub.description}</div>
          {/if}

          <div class="sub-meta">
            <span class="sub-amount"><strong>{sub.amount}</strong> {$currentNetwork.symbol}</span>
            <span class="sep">·</span>
            <span class="sub-schedule text-muted">{sub.schedule}</span>

            {#if sub.canary_status === "Expired"}
              <span class="sep">·</span>
              <span class="badge badge-red">{$t.canaryExpired}</span>
            {:else if sub.canary_status === "Unreachable"}
              <span class="sep">·</span>
              <span class="badge badge-orange">{$t.canaryAlert}</span>
            {/if}
          </div>

          <div class="sub-footer">
            <span class="text-muted sent-count">{sub.payments_sent} {$t.sent}</span>

            <!-- Wallet badge -->
            {#if walletName(sub.wallet_id)}
              <span class="wallet-badge" style="border-color:{walletColor(sub.wallet_id)}; color:{walletColor(sub.wallet_id)}">
                {$t.fromWallet}: {walletName(sub.wallet_id)}
              </span>
            {/if}

            <!-- NEXT date only if not suspended -->
            {#if !isSuspended(sub) && sub.next_payment_at}
              <span class="next-date">
                <span class="text-muted">{$t.next}</span>
                <strong>{formatDate(sub.next_payment_at)}</strong>
              </span>
              <!-- Pie countdown timer -->
              <span class="pie-wrap" title={countdownText(sub, now)}>
                <svg width="24" height="24" viewBox="0 0 24 24">
                  <circle cx="12" cy="12" r="10" fill="var(--bg-sidebar)" stroke="var(--border)" stroke-width="1.5"/>
                  <path d={piePath(pieFraction(sub, now))} fill="var(--accent)" opacity="0.8"/>
                  <circle cx="12" cy="12" r="10" fill="none" stroke="var(--border)" stroke-width="1.5"/>
                </svg>
                <span class="countdown-label">{countdownText(sub, now)}</span>
              </span>
            {:else if isSuspended(sub)}
              <span class="badge badge-red suspended-badge">{$t.suspendedNoNext}</span>
            {/if}
          </div>
        </div>

        <div class="sub-actions">
          <!-- Play / Pause -->
          {#if !isSuspended(sub)}
            <button class="btn-ghost btn-sm" title={sub.status === "Paused" ? "Reprendre" : "Mettre en pause"}
              on:click={e => togglePause(sub, e)}>
              {sub.status === "Paused" ? "▶" : "⏸"}
            </button>
          {/if}

          <!-- Kebab menu -->
          <div class="kebab-wrap">
            <button class="btn-ghost btn-sm" on:click={e => toggleMenu(sub.id, e)}>⋯</button>
            {#if openMenuId === sub.id}
              <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
              <div class="kebab-menu" on:click|stopPropagation>
                {#if !isSuspended(sub)}
                  <button class="kebab-item kebab-trigger" on:click={e => triggerNow(sub, e)}>
                    ▶ Declencher maintenant
                  </button>
                  <button class="kebab-item" on:click={e => editSub(sub, e)}>
                    ✏ Modifier
                  </button>
                  <button class="kebab-item" on:click={e => togglePause(sub, e)}>
                    {sub.status === "Paused" ? "▶ Reprendre" : "⏸ Pause"}
                  </button>
                {/if}
                <button class="kebab-item kebab-danger" on:click={e => deleteSub(sub, e)}>
                  Supprimer
                </button>
              </div>
            {/if}
          </div>
        </div>
      </div>
    {/each}
  {/if}
</div>

<style>
  .sub-list { display: flex; flex-direction: column; gap: 8px; flex: 1; }

  .empty-state { padding: 28px; text-align: center; display: flex; flex-direction: column; gap: 6px; }

  .sub-card {
    display: flex; align-items: flex-start; gap: 12px; padding: 13px 14px;
    transition: background .15s;
  }
  .sub-card:hover { background: var(--bg-card-hover); }
  .sub-archived { opacity: 0.5; }
  .sub-suspended { opacity: 0.72; }

  .sub-left { display: flex; align-items: flex-start; padding-top: 4px; }

  .sub-body { flex: 1; display: flex; flex-direction: column; gap: 4px; min-width: 0; }

  .sub-header { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .sub-label { font-weight: 700; font-size: 14px; color: var(--text); }
  .sub-desc { font-size: 12px; }

  .sub-meta { display: flex; align-items: center; gap: 6px; font-size: 13px; flex-wrap: wrap; }
  .sub-amount { color: var(--text); }
  .sep { color: var(--border); font-size: 16px; }

  .sub-footer { display: flex; align-items: center; gap: 8px; margin-top: 2px; flex-wrap: wrap; }
  .sent-count { font-size: 11px; }

  .wallet-badge {
    font-size: 10px;
    padding: 1px 7px;
    border-radius: 10px;
    border: 1px solid;
    opacity: 0.75;
  }

  .next-date {
    display: flex; align-items: center; gap: 5px; font-size: 12px;
    background: var(--accent-light); padding: 2px 8px;
    border-radius: 12px; color: var(--accent);
  }

  .suspended-badge { font-size: 11px; }

  .pie-wrap {
    display: flex; align-items: center; gap: 4px;
    cursor: default;
  }
  .countdown-label {
    font-size: 11px; color: var(--text-muted); white-space: nowrap;
  }
  .kebab-trigger { color: var(--accent); }
  .kebab-trigger:hover { background: var(--accent-light); }

  .sub-actions { display: flex; flex-direction: column; gap: 3px; align-items: flex-end; }

  /* Kebab menu */
  .kebab-wrap { position: relative; }
  .kebab-menu {
    position: absolute;
    right: 0; top: calc(100% + 4px);
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 4px 16px #0004;
    min-width: 150px;
    z-index: 100;
    overflow: hidden;
  }
  .kebab-item {
    display: block; width: 100%;
    padding: 9px 14px;
    text-align: left;
    font-size: 0.85rem;
    background: none; border: none; cursor: pointer;
    color: var(--text);
    transition: background 0.12s;
  }
  .kebab-item:hover { background: var(--bg-card-hover); }
  .kebab-danger { color: #ef4444; }
  .kebab-danger:hover { background: #ef444410; }
  .btn-sm { padding: 5px 10px; font-size: 13px; }
</style>
