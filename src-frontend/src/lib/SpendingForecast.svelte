<script>
  import { createEventDispatcher } from "svelte";
  import { t, fmt, currentLang } from "./i18n.js";

  /** Result of `get_spending_forecast`, or null while loading. */
  export let forecast = null;
  export let symbol = "XMR";
  export let paused = false;

  const dispatch = createEventDispatcher();
  const RANGES = [30, 90, 180, 365];
  const W = 600, H = 170;

  $: days = forecast?.days ?? 180;
  $: balance = forecast?.balance_xmr ?? null;
  $: spend = forecast?.total_xmr ?? 0;

  // One value per day: projected balance (or cumulative spend when the balance is unknown).
  $: series = (() => {
    if (!forecast) return [];
    const start = new Date();
    start.setHours(0, 0, 0, 0);
    const byDate = new Map(forecast.per_day.map(d => [d.date, d.xmr]));
    let cum = 0;
    const out = [];
    for (let i = 0; i <= days; i++) {
      const d = new Date(start.getTime() + i * 86400000);
      const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
      cum += byDate.get(key) || 0;
      out.push({ d, v: balance !== null ? balance - cum : cum });
    }
    return out;
  })();

  $: yMax = Math.max(1e-9, ...series.map(p => p.v), balance ?? 0);
  $: yMin = Math.min(0, ...series.map(p => p.v));
  const x = (i) => (i / Math.max(1, days)) * W;
  $: y = (v) => H - ((v - yMin) / (yMax - yMin || 1)) * H;
  $: line = series.map((p, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(1)},${y(p.v).toFixed(1)}`).join(" ");
  $: area = series.length ? `${line} L${W},${y(yMin)} L0,${y(yMin)} Z` : "";
  $: zeroY = y(0);
  $: emptyIdx = forecast?.empty_at
    ? Math.round((new Date(forecast.empty_at) - series[0]?.d) / 86400000)
    : null;

  $: subs = (forecast?.per_sub ?? []).slice().sort((a, b) => b.xmr - a.xmr);
  $: subMax = Math.max(1e-9, ...subs.map(s => s.xmr));

  const dateFmt = (d, lang) => d.toLocaleDateString(lang, { day: "numeric", month: "short" });
  const xmr = (v) => (v >= 0.1 ? v.toFixed(3) : v.toFixed(4));
</script>

<section class="forecast card">
  <div class="forecast-head">
    <h2 class="section-title">{$t.forecastTitle}</h2>
    <div class="forecast-ranges" role="group" aria-label={$t.forecastTitle}>
      {#each RANGES as r}
        <button class:active={r === days} on:click={() => dispatch("range", r)}>{fmt($t.forecastRange, { n: r })}</button>
      {/each}
    </div>
  </div>
  <p class="text-muted forecast-desc">{$t.forecastDesc}{#if paused}{" "}{$t.forecastPaused}{/if}</p>

  {#if !forecast}
    <p class="text-muted forecast-desc">…</p>
  {:else if forecast.per_day.length === 0}
    <p class="text-muted forecast-desc">{$t.forecastNoSubs}</p>
  {:else}
    <div class="forecast-facts">
      {#if balance !== null}
        <div><span class="text-muted">{$t.forecastBalanceNow}</span><strong>{xmr(balance)} {symbol}</strong></div>
      {/if}
      <div><span class="text-muted">{fmt($t.forecastSpend, { n: days })}</span><strong>−{xmr(spend)} {symbol}</strong></div>
      {#if balance !== null}
        {#if forecast.empty_at}
          <div class="forecast-alert"><span class="text-muted">{$t.forecastOutcome}</span>
            <strong>{fmt($t.forecastEmptyOn, { date: dateFmt(new Date(forecast.empty_at), $currentLang) })}</strong></div>
        {:else}
          <div><span class="text-muted">{$t.forecastOutcome}</span><strong>{xmr(balance - spend)} {symbol} · {$t.forecastEnough}</strong></div>
        {/if}
      {/if}
    </div>
    {#if balance === null}<p class="text-muted forecast-desc">{$t.forecastBalanceUnknown}</p>{/if}

    <div class="forecast-chart">
      <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" role="img"
        aria-label={balance !== null ? $t.forecastBalanceNow : $t.forecastCumulative}>
        <path d={area} class="fc-area" class:fc-neg={forecast.empty_at} />
        {#if yMin < 0}<line x1="0" x2={W} y1={zeroY} y2={zeroY} class="fc-zero" />{/if}
        <path d={line} class="fc-line" vector-effect="non-scaling-stroke" />
        {#if emptyIdx !== null && emptyIdx >= 0 && emptyIdx <= days}
          <line x1={x(emptyIdx)} x2={x(emptyIdx)} y1="0" y2={H} class="fc-empty" vector-effect="non-scaling-stroke" />
        {/if}
      </svg>
      <div class="fc-axis text-muted">
        <span>{$t.forecastToday}</span>
        <span>{series[Math.round(days / 2)] ? dateFmt(series[Math.round(days / 2)].d, $currentLang) : ""}</span>
        <span>{series[days] ? dateFmt(series[days].d, $currentLang) : ""}</span>
      </div>
    </div>

    <ul class="forecast-subs">
      {#each subs as s (s.id)}
        <li>
          <span class="fs-label">{s.label}</span>
          <span class="fs-bar"><span style="width:{(s.xmr / subMax) * 100}%"></span></span>
          <span class="fs-num text-muted">{fmt($t.forecastPayments, { n: s.payments })}</span>
          <span class="fs-num">{xmr(s.xmr)} {symbol}</span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .forecast { margin-top: 22px; padding: 16px 18px; }
  .forecast-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; flex-wrap: wrap; }
  .forecast-head .section-title { margin: 0; }
  .forecast-ranges { display: inline-flex; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }
  .forecast-ranges button {
    background: none; border: none; padding: 4px 10px; font: inherit; font-size: 12px;
    color: var(--text-muted); cursor: pointer;
  }
  .forecast-ranges button + button { border-left: 1px solid var(--border); }
  .forecast-ranges button.active { background: var(--bg-hover); color: var(--text); font-weight: 600; }
  .forecast-desc { font-size: 12px; margin: 6px 0 0; line-height: 1.5; }
  .forecast-facts { display: flex; flex-wrap: wrap; gap: 8px 26px; margin: 14px 0 10px; font-size: 13px; }
  .forecast-facts div { display: flex; flex-direction: column; gap: 2px; }
  .forecast-facts span { font-size: 11.5px; }
  .forecast-alert strong { color: #ea580c; }
  .forecast-chart { margin-top: 6px; }
  .forecast-chart svg { width: 100%; height: 170px; display: block; overflow: visible; }
  .fc-area { fill: var(--accent); opacity: .12; }
  .fc-area.fc-neg { fill: #ea580c; }
  .fc-line { fill: none; stroke: var(--accent); stroke-width: 2; }
  .fc-zero { stroke: var(--border); stroke-dasharray: 4 4; }
  .fc-empty { stroke: #ea580c; stroke-width: 1.5; stroke-dasharray: 3 3; }
  .fc-axis { display: flex; justify-content: space-between; font-size: 11px; margin-top: 4px; }
  .forecast-subs { list-style: none; margin: 14px 0 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .forecast-subs li { display: grid; grid-template-columns: minmax(80px, 1.2fr) 2fr auto auto; gap: 10px; align-items: center; font-size: 12.5px; }
  .fs-label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fs-bar { height: 6px; background: var(--bg-hover); border-radius: 3px; overflow: hidden; }
  .fs-bar span { display: block; height: 100%; background: var(--accent); opacity: .7; border-radius: 3px; }
  .fs-num { white-space: nowrap; text-align: right; font-variant-numeric: tabular-nums; }
</style>
