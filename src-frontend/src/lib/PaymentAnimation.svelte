<script>
  import { onMount, onDestroy } from "svelte";
  import { t } from "./i18n.js";
  import { currentNetwork } from "./network.js";

  // Subjects: array of { toLabel, xmrAmount, btcAmount }
  // Cycles every 7s, matching the CSS animation duration
  export let subjects = [
    { toLabel: null, xmrAmount: "0.05", btcAmount: "420 sats" },
    { toLabel: null, xmrAmount: "0.012", btcAmount: "120 sats" },
  ];

  let cycleIdx = 0;
  let interval;

  const CYCLE_DURATION = 7000; // ms — must match CSS animation duration

  onMount(() => {
    interval = setInterval(() => {
      cycleIdx = (cycleIdx + 1) % subjects.length;
    }, CYCLE_DURATION);
  });
  onDestroy(() => clearInterval(interval));

  $: current = subjects[cycleIdx];
  $: toLabel = current.toLabel || (cycleIdx === 0 ? $t.mediaLabel : $t.vpnLabel);
  $: amount = ($currentNetwork.id === "bitcoin" ? current.btcAmount : current.xmrAmount)
              + " " + $currentNetwork.symbol + ($t.animSuffix);
  $: fromLabel = $t.myWallet;
  $: dm = $t.dayMinus;
</script>

<div class="anim-wrap" aria-hidden="true">
  <svg viewBox="0 0 280 90" xmlns="http://www.w3.org/2000/svg">
    <!-- Dashed connection line -->
    <line x1="58" y1="43" x2="222" y2="43"
          stroke="var(--border)" stroke-width="1.5" stroke-dasharray="5 4"/>

    <!-- Wallet A -->
    <g>
      <rect x="8" y="25" width="44" height="36" rx="6"
            fill="var(--bg-card)" stroke="var(--border)" stroke-width="1.5"/>
      <rect x="18" y="33" width="12" height="9" rx="2"
            fill="var(--accent)" opacity="0.75"/>
      <line x1="18" y1="50" x2="42" y2="50" stroke="var(--border)" stroke-width="1.5"/>
      <line x1="18" y1="55" x2="36" y2="55" stroke="var(--border)" stroke-width="1"/>
    </g>

    <!-- Traveling dot -->
    <circle class="dot" cx="30" cy="43" r="5" fill="var(--accent)"/>

    <!-- Timer ring background -->
    <circle cx="140" cy="43" r="20" fill="none" stroke="var(--border)" stroke-width="2.5"/>
    <!-- Timer ring fill -->
    <circle class="ring" cx="140" cy="43" r="20"
            fill="none" stroke="var(--accent)" stroke-width="2.5"
            stroke-dasharray="125.7" stroke-dashoffset="125.7"
            stroke-linecap="round"
            transform="rotate(-90 140 43)"/>

    <!-- Countdown labels: J-3, J-2, J-1, ✓ -->
    <text class="n3" x="140" y="48" text-anchor="middle"
          fill="var(--accent)" font-size="12" font-weight="700" font-family="sans-serif">{dm}3</text>
    <text class="n2" x="140" y="48" text-anchor="middle"
          fill="var(--accent)" font-size="12" font-weight="700" font-family="sans-serif">{dm}2</text>
    <text class="n1" x="140" y="48" text-anchor="middle"
          fill="var(--accent)" font-size="12" font-weight="700" font-family="sans-serif">{dm}1</text>
    <text class="n0" x="140" y="48" text-anchor="middle"
          fill="var(--green)" font-size="14" font-weight="700" font-family="sans-serif">✓</text>

    <!-- Wallet B -->
    <g>
      <rect class="wallet-b-rect" x="228" y="25" width="44" height="36" rx="6"
            fill="var(--bg-card)" stroke="var(--border)" stroke-width="1.5"/>
      <rect x="238" y="33" width="12" height="9" rx="2"
            fill="var(--accent)" opacity="0.35"/>
      <line x1="238" y1="50" x2="262" y2="50" stroke="var(--border)" stroke-width="1.5"/>
      <line x1="238" y1="55" x2="255" y2="55" stroke="var(--border)" stroke-width="1"/>
    </g>

    <!-- Labels (reactive, change every 7s with cycleIdx) -->
    <text x="30" y="72" text-anchor="middle"
          font-size="8.5" fill="var(--text-muted)" font-family="sans-serif">{fromLabel}</text>
    <text x="250" y="72" text-anchor="middle"
          font-size="8.5" fill="var(--text-muted)" font-family="sans-serif">{toLabel}</text>
    <text x="140" y="82" text-anchor="middle"
          font-size="8.5" fill="var(--accent)" font-family="sans-serif">{amount}</text>
  </svg>
</div>

<style>
  .anim-wrap {
    width: 100%;
    max-width: 280px;
    margin: 0 auto;
  }

  svg { overflow: visible; }

  /* ---- Dot: travels A→B in first 21% of 7s cycle ---- */
  .dot {
    animation: dot-travel 7s ease-in-out infinite;
    transform-origin: 30px 43px;
  }
  @keyframes dot-travel {
    0%   { transform: translateX(0);     opacity: 0; }
    3%   { opacity: 1; }
    21%  { transform: translateX(220px); opacity: 1; }
    24%  { transform: translateX(220px); opacity: 0; }
    99%  { transform: translateX(0);     opacity: 0; }
    100% { transform: translateX(0);     opacity: 0; }
  }

  /* ---- Ring: fills from 25% to 68% (3s countdown) ---- */
  .ring {
    animation: ring-fill 7s linear infinite;
  }
  @keyframes ring-fill {
    0%,  25%  { stroke-dashoffset: 125.7; opacity: 0.9; }
    68%       { stroke-dashoffset: 0;     opacity: 1; }
    71%       { stroke-dashoffset: 0;     opacity: 1; }
    75%       { stroke-dashoffset: 125.7; opacity: 0; }
    100%      { stroke-dashoffset: 125.7; opacity: 0; }
  }

  /* ---- Countdown: J-3 from 25%→39%, J-2 from 39%→53%, J-1 from 53%→67%, ✓ from 67%→74% ---- */
  .n3 { animation: n3-anim 7s infinite; }
  @keyframes n3-anim {
    0%, 24.9%, 39%, 100% { opacity: 0; }
    25%, 38.9%            { opacity: 1; }
  }
  .n2 { animation: n2-anim 7s infinite; }
  @keyframes n2-anim {
    0%, 38.9%, 53%, 100% { opacity: 0; }
    39%, 52.9%            { opacity: 1; }
  }
  .n1 { animation: n1-anim 7s infinite; }
  @keyframes n1-anim {
    0%, 52.9%, 67%, 100% { opacity: 0; }
    53%, 66.9%            { opacity: 1; }
  }
  .n0 { animation: n0-anim 7s infinite; }
  @keyframes n0-anim {
    0%, 66.9%, 75%, 100% { opacity: 0; }
    68%, 74%              { opacity: 1; }
  }

  /* ---- Wallet B glow on receive ---- */
  .wallet-b-rect {
    animation: wb-glow 7s infinite;
  }
  @keyframes wb-glow {
    0%, 19%, 26%, 100% { stroke: var(--border); }
    21%, 24%            { stroke: var(--accent); filter: drop-shadow(0 0 5px var(--accent)); }
  }
</style>
