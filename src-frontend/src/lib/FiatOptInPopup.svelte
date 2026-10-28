<!--
  Privacy warning shown before the first price request. Self-contained (own strings)
  because it is shared with the web generator, which has no i18n store.
  Events: on:confirm, on:cancel
-->
<script>
  import { createEventDispatcher } from "svelte";

  export let lang = "fr";
  const dispatch = createEventDispatcher();

  const TEXT = {
    fr: {
      title: "Activation de la conversion fiat",
      body: "L'API LibreMonero.com sera utilisée pour récupérer le cours actuel. C'est un service bénévole qui garantit l'anonymat et ne conserve aucun log.",
      hint: "Pour un anonymat maximal, il est recommandé de laisser ce champ désactivé.",
      enable: "Activer", cancel: "Annuler",
    },
    en: {
      title: "Enable fiat conversion",
      body: "The LibreMonero.com API will be used to fetch the current rate. It is a volunteer service that guarantees anonymity and keeps no logs.",
      hint: "For maximum anonymity, it is recommended to leave this field disabled.",
      enable: "Enable", cancel: "Cancel",
    },
    de: {
      title: "Fiat-Umrechnung aktivieren",
      body: "Die API von LibreMonero.com wird verwendet, um den aktuellen Kurs abzurufen. Es ist ein ehrenamtlicher Dienst, der Anonymität garantiert und keine Logs speichert.",
      hint: "Für maximale Anonymität wird empfohlen, diese Option deaktiviert zu lassen.",
      enable: "Aktivieren", cancel: "Abbrechen",
    },
    es: {
      title: "Activar la conversión fiat",
      body: "Se usará la API de LibreMonero.com para obtener el cambio actual. Es un servicio voluntario que garantiza el anonimato y no guarda registros.",
      hint: "Para un anonimato máximo, se recomienda dejar esta opción desactivada.",
      enable: "Activar", cancel: "Cancelar",
    },
  };
  $: tx = TEXT[lang] || TEXT.en;
</script>

<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
<div class="fiat-popup-overlay" on:click={() => dispatch("cancel")}>
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fiat-popup card" on:click|stopPropagation>
    <p class="fiat-popup-title">⚠ {tx.title}</p>
    <p class="fiat-popup-body">{tx.body}</p>
    <p class="fiat-popup-body fiat-popup-hint">{tx.hint}</p>
    <div class="fiat-popup-actions">
      <button class="btn-primary" type="button" on:click={() => dispatch("confirm")}>{tx.enable}</button>
      <button class="btn-ghost" type="button" on:click={() => dispatch("cancel")}>{tx.cancel}</button>
    </div>
  </div>
</div>

<style>
  .fiat-popup-overlay {
    position: fixed; inset: 0; z-index: 300;
    display: flex; align-items: center; justify-content: center;
    background: #0005;
  }
  .fiat-popup {
    max-width: 320px; width: 90%; padding: 1.25rem;
    display: flex; flex-direction: column; gap: 10px;
  }
  .fiat-popup-title { font-weight: 700; font-size: 0.95rem; margin: 0; }
  .fiat-popup-body { font-size: 0.84rem; line-height: 1.5; margin: 0; }
  .fiat-popup-hint { font-style: italic; color: var(--text-muted); }
  .fiat-popup-actions { display: flex; gap: 8px; }
</style>
