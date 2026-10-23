import { writable, derived } from "svelte/store";

export const NETWORKS = [
  { id: "monero", label: "Monero", symbol: "XMR", color: "#f26822" },
  { id: "bitcoin", label: "Bitcoin", symbol: "BTC", color: "#f7931a" },
];

export const selectedNetwork = writable("monero");
export const currentNetwork = derived(selectedNetwork, $id => NETWORKS.find(n => n.id === $id) || NETWORKS[0]);
