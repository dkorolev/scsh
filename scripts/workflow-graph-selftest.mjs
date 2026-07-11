#!/usr/bin/env node
// Parity smoke for job-graph layout helpers (mirrors client_js wfNodeRanks / wfLayoutNodes).
// Run: node scripts/workflow-graph-selftest.mjs
'use strict';

const WF_NODE_W = 200, WF_NODE_H = 72, WF_GAP_X = 56, WF_GAP_Y = 28, WF_PAD = 16;

function wfNodeRanks(nodes) {
  const byId = Object.fromEntries(nodes.map(n => [n.id, n]));
  const ranks = Object.create(null);
  function rankOf(id) {
    if (ranks[id] != null) return ranks[id];
    const node = byId[id];
    if (!node) { ranks[id] = 0; return 0; }
    const needs = node.needs || [];
    const r = needs.length ? (1 + Math.max(...needs.map(rankOf))) : 0;
    ranks[id] = r;
    return r;
  }
  return nodes.map(n => rankOf(n.id));
}

const arith = [
  { id: 'add', order: 0, needs: [] },
  { id: 'multiply', order: 1, needs: [] },
  { id: 'summarize', order: 2, needs: ['add', 'multiply'] },
];
const ranks = wfNodeRanks(arith);
if (JSON.stringify(ranks) !== JSON.stringify([0, 0, 1])) {
  console.error('FAIL ranks', ranks);
  process.exit(1);
}
console.log('ok workflow-graph-selftest ranks', ranks);
