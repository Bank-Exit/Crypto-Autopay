<!--
  autopay.libre-market.com — Landing + code generator
  Reuses SubscriptionCodeEditor from the Tauri app (shared via $lib alias)
-->
<script>
  import SubscriptionCodeEditor from "$lib/SubscriptionCodeEditor.svelte";

  let lang = "fr";
  const LANGS = [{ code: "fr", label: "Français" }, { code: "en", label: "English" }];

  let page = "home"; // "home" | "generator" | "spec" | "decode"

  // Route from URL hash
  if (typeof window !== "undefined") {
    const hash = window.location.hash.replace("#", "");
    if (["generator", "spec", "decode"].includes(hash)) page = hash;
    window.addEventListener("hashchange", () => {
      const h = window.location.hash.replace("#", "");
      if (["generator", "spec", "decode"].includes(h)) page = h;
      else page = "home";
    });
  }

  function nav(p) {
    page = p;
    if (typeof window !== "undefined") window.location.hash = p === "home" ? "" : p;
  }

  function onGenerated({ detail }) {
    // stay on page, the component shows the output inline
  }

  function onImported({ detail }) {
    // stay on page
  }
</script>

<div class="site" data-lang={lang}>
  <!-- Nav -->
  <header class="site-nav">
    <div class="nav-inner">
      <button class="nav-logo" on:click={() => nav("home")}>
        <svg width="22" height="22" viewBox="0 0 72 72" fill="none">
          <path d="M36 8 A28 28 0 1 1 8 36" stroke="#e8601a" stroke-width="5" stroke-linecap="round" fill="none"/>
          <polyline points="6,24 8,36 20,34" stroke="#e8601a" stroke-width="5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
          <circle cx="24" cy="36" r="5" fill="#e8601a" opacity="0.9"/>
          <circle cx="48" cy="36" r="5" fill="#e8601a" opacity="0.5"/>
          <line x1="31" y1="36" x2="41" y2="36" stroke="#e8601a" stroke-width="2.5" stroke-linecap="round"/>
          <polyline points="38,32 42,36 38,40" stroke="#e8601a" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>
        </svg>
        <span>Crypto AutoPay</span>
      </button>
      <nav class="nav-links">
        <button class:active={page === "generator"} on:click={() => nav("generator")}>
          {lang === "fr" ? "Créer un code" : "Create a code"}
        </button>
        <button class:active={page === "decode"} on:click={() => nav("decode")}>
          {lang === "fr" ? "Décoder" : "Decode"}
        </button>
        <button class:active={page === "spec"} on:click={() => nav("spec")}>Spec</button>
        <a href="https://github.com/youorg/crypto-autopay" target="_blank" rel="noreferrer">GitHub ↗</a>
      </nav>
      <select bind:value={lang} class="lang-sel">
        {#each LANGS as l}<option value={l.code}>{l.label}</option>{/each}
      </select>
    </div>
  </header>

  <main class="site-main">
    <!-- ====== HOME ====== -->
    {#if page === "home"}
      <section class="hero">
        <h1 class="hero-title">
          {lang === "fr" ? "Abonnements crypto" : "Crypto subscriptions"}<br/>
          <span class="accent">{lang === "fr" ? "sans intermédiaire" : "without middlemen"}</span>
        </h1>
        <p class="hero-sub">
          {lang === "fr"
            ? "Créez, partagez et gérez des abonnements récurrents en Monero, Bitcoin, USDT et plus — sans serveur central, sans données bancaires, sans frais cachés."
            : "Create, share, and manage recurring payments in Monero, Bitcoin, USDT and more — no central server, no card data, no hidden fees."}
        </p>
        <div class="hero-ctas">
          <button class="btn-primary btn-lg" on:click={() => nav("generator")}>
            {lang === "fr" ? "Créer un code abonnement →" : "Create a subscription code →"}
          </button>
          <a href="https://github.com/youorg/crypto-autopay/releases" class="btn-secondary btn-lg" target="_blank" rel="noreferrer">
            {lang === "fr" ? "Télécharger le wallet" : "Download the wallet"}
          </a>
        </div>
      </section>

      <section class="features-grid">
        {#each [
          { icon: "🔒", title: lang === "fr" ? "Non-custodial" : "Non-custodial",
            desc: lang === "fr" ? "Votre clé privée ne quitte jamais votre appareil." : "Your private key never leaves your device." },
          { icon: "⛓", title: lang === "fr" ? "Multi-réseau" : "Multi-network",
            desc: lang === "fr" ? "Monero, Bitcoin, USDT (Ethereum/Tron/Solana), et plus." : "Monero, Bitcoin, USDT (Ethereum/Tron/Solana), and more." },
          { icon: "🦅", title: "Canary",
            desc: lang === "fr" ? "Signal signé permettant à un marchand d'arrêter tous ses abonnements en cas de compromission." : "Signed signal allowing merchants to stop all subscriptions if compromised." },
          { icon: "🤖", title: lang === "fr" ? "Agents IA" : "AI agents",
            desc: lang === "fr" ? "Protocole machine-readable avec balises et ToS pour les agents autonomes." : "Machine-readable protocol with tags and ToS for autonomous agents." },
          { icon: "📅", title: lang === "fr" ? "Cron universel" : "Universal cron",
            desc: lang === "fr" ? "Hebdomadaire, mensuel, annuel, tous les 3 ans… n'importe quelle fréquence." : "Weekly, monthly, yearly, every 3 years… any frequency." },
          { icon: "🧬", title: lang === "fr" ? "Post-quantique ready" : "Post-quantum ready",
            desc: lang === "fr" ? "Signatures Ed25519 aujourd'hui, ML-DSA-65 (Dilithium) pour le futur." : "Ed25519 signatures today, ML-DSA-65 (Dilithium) for the future." },
        ] as f}
          <div class="feature-card">
            <div class="feature-icon">{f.icon}</div>
            <h3>{f.title}</h3>
            <p>{f.desc}</p>
          </div>
        {/each}
      </section>

      <section class="format-example">
        <h2>{lang === "fr" ? "Le format" : "The format"}</h2>
        <p>{lang === "fr" ? "Un code — un QR, un lien, ou du texte brut :" : "One code — one QR, a link, or plain text:"}</p>
        <pre class="code-sample">crypto-autopay:1:H4sIAAAAAAAAA6tWKkktLlGyUlIqS04tLk5MTQUAVfAHlhAAAAA=</pre>
        <p class="code-note">
          {lang === "fr"
            ? "Base64url(gzip(JSON canonique)) — standard ouvert, zéro dépendance serveur."
            : "Base64url(gzip(canonical JSON)) — open standard, zero server dependency."}
        </p>
        <button class="btn-outline" on:click={() => nav("spec")}>
          {lang === "fr" ? "Lire la spécification complète →" : "Read the full specification →"}
        </button>
      </section>

    <!-- ====== GENERATOR / DECODE ====== -->
    {:else if page === "generator" || page === "decode"}
      <section class="generator-section">
        <div class="generator-header">
          <h1>{lang === "fr" ? "Générateur de code" : "Code generator"}</h1>
          <p>
            {lang === "fr"
              ? "Créez un code crypto-autopay:1: à partager avec vos abonnés — sous forme de QR, lien ou texte."
              : "Create a crypto-autopay:1: code to share with your subscribers — as a QR, link, or text."}
          </p>
        </div>
        <div class="editor-wrapper card">
          <SubscriptionCodeEditor
            mode={page === "decode" ? "import" : "create"}
            {lang}
            on:generated={onGenerated}
            on:imported={onImported}
            on:close={() => nav("home")}
          />
        </div>
      </section>

    <!-- ====== SPEC ====== -->
    {:else if page === "spec"}
      <section class="spec-section">
        <h1>Specification — crypto-autopay:1:</h1>
        <p>
          {lang === "fr"
            ? "La spécification complète est disponible dans le dépôt Git."
            : "The full specification is available in the Git repository."}
        </p>
        <a href="https://github.com/youorg/crypto-autopay/blob/main/SPEC.md" class="btn-primary" target="_blank" rel="noreferrer">
          SPEC.md ↗
        </a>
      </section>
    {/if}
  </main>

  <footer class="site-footer">
    <div class="footer-inner">
      <span>Crypto AutoPay — {lang === "fr" ? "Protocole ouvert" : "Open protocol"} · GPL-3.0</span>
      <span>·</span>
      <a href="https://autopay.libre-market.com/spec/v1">{lang === "fr" ? "Spécification v1" : "Spec v1"}</a>
      <span>·</span>
      <a href="https://libre-market.com">Libre-Market</a>
    </div>
  </footer>
</div>

<style>
  :global(*, *::before, *::after) { box-sizing: border-box; }
  :global(body) {
    margin: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    background: #f4f1ed; color: #1a1612; line-height: 1.6;
  }
  :global(a) { color: #e8601a; }
  :global(input, select, textarea) { font-family: inherit; }

  /* Nav */
  .site-nav {
    position: sticky; top: 0; z-index: 100;
    background: #f4f1ed; border-bottom: 1px solid #e2ddd7;
  }
  .nav-inner {
    max-width: 900px; margin: 0 auto; padding: 0 20px;
    display: flex; align-items: center; gap: 20px; height: 52px;
  }
  .nav-logo {
    display: flex; align-items: center; gap: 8px;
    background: none; border: none; cursor: pointer;
    font-size: 15px; font-weight: 700; color: #1a1612; padding: 0;
  }
  .nav-links { display: flex; gap: 4px; flex: 1; }
  .nav-links button, .nav-links a {
    background: none; border: none; padding: 6px 12px; font-size: 13px;
    cursor: pointer; color: #666; border-radius: 6px;
    text-decoration: none; transition: background .12s;
  }
  .nav-links button:hover, .nav-links a:hover { background: #eae6e0; color: #1a1612; }
  .nav-links button.active { background: #eae6e0; color: #e8601a; font-weight: 600; }
  .lang-sel {
    border: 1px solid #ddd; border-radius: 6px; padding: 4px 8px;
    font-size: 12px; background: white; color: #333; cursor: pointer;
  }

  /* Main */
  .site-main { max-width: 900px; margin: 0 auto; padding: 40px 20px 80px; }

  /* Hero */
  .hero { text-align: center; padding: 60px 20px 40px; }
  .hero-title { font-size: clamp(28px, 5vw, 48px); font-weight: 800; line-height: 1.15; margin: 0 0 16px; }
  .accent { color: #e8601a; }
  .hero-sub { font-size: 16px; color: #555; max-width: 580px; margin: 0 auto 28px; }
  .hero-ctas { display: flex; gap: 12px; justify-content: center; flex-wrap: wrap; }

  /* Features */
  .features-grid {
    display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 16px; margin: 48px 0;
  }
  .feature-card {
    background: white; border-radius: 12px; padding: 20px;
    border: 1px solid #e2ddd7;
  }
  .feature-icon { font-size: 24px; margin-bottom: 10px; }
  .feature-card h3 { font-size: 14px; font-weight: 700; margin: 0 0 6px; }
  .feature-card p { font-size: 12.5px; color: #666; margin: 0; }

  /* Format example */
  .format-example {
    background: white; border-radius: 14px; padding: 28px;
    border: 1px solid #e2ddd7; text-align: center;
  }
  .format-example h2 { font-size: 20px; margin: 0 0 8px; }
  .code-sample {
    background: #f8f5f0; border: 1px solid #e2ddd7; border-radius: 8px;
    padding: 14px 16px; font-family: "Courier New", monospace; font-size: 12px;
    word-break: break-all; text-align: left; margin: 16px 0 8px;
  }
  .code-note { font-size: 12px; color: #888; margin: 0 0 16px; }

  /* Generator */
  .generator-section { max-width: 660px; margin: 0 auto; }
  .generator-header { text-align: center; margin-bottom: 24px; }
  .generator-header h1 { font-size: 24px; margin: 0 0 8px; }
  .generator-header p { font-size: 14px; color: #666; margin: 0; }
  .editor-wrapper {
    background: white; border-radius: 14px;
    border: 1px solid #e2ddd7; padding: 0; overflow: hidden;
  }
  .editor-wrapper :global(.sce) { padding: 24px; }

  /* Buttons */
  .btn-primary {
    background: #e8601a; color: white; border: none;
    border-radius: 9px; padding: 10px 18px; font-size: 14px; font-weight: 600;
    cursor: pointer; text-decoration: none; display: inline-block;
    transition: opacity .15s;
  }
  .btn-primary:hover { opacity: 0.88; }
  .btn-secondary {
    background: white; color: #1a1612; border: 2px solid #1a1612;
    border-radius: 9px; padding: 10px 18px; font-size: 14px; font-weight: 600;
    cursor: pointer; text-decoration: none; display: inline-block;
    transition: background .15s;
  }
  .btn-secondary:hover { background: #f0ece7; }
  .btn-outline {
    background: transparent; color: #e8601a; border: 2px solid #e8601a;
    border-radius: 9px; padding: 9px 16px; font-size: 13px; font-weight: 600;
    cursor: pointer;
  }
  .btn-lg { padding: 13px 22px; font-size: 15px; }

  /* Spec page */
  .spec-section { text-align: center; padding: 60px 20px; }
  .spec-section h1 { font-size: 24px; margin: 0 0 12px; }

  /* Footer */
  .site-footer {
    border-top: 1px solid #e2ddd7; background: #ede9e4;
    padding: 16px 20px;
  }
  .footer-inner {
    max-width: 900px; margin: 0 auto;
    display: flex; gap: 10px; align-items: center;
    font-size: 12px; color: #888; flex-wrap: wrap;
  }
  .footer-inner a { color: #888; }

  /* Responsive */
  @media (max-width: 600px) {
    .hero { padding: 32px 0 24px; }
    .hero-ctas { flex-direction: column; align-items: stretch; }
    .nav-links button:not(.active) { display: none; }
  }
</style>
