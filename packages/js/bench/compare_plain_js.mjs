// SDK vs plain JavaScript on the same mapping: hand-written map, a ~25-line
// generic recipe interpreter, and the WASM SDK. Also checks the outputs match.
// Usage: node packages/js/bench/compare_plain_js.mjs [records=100000]
import { DTOEngine } from '../src/index.js';
import assert from 'node:assert/strict';

const count = Number(process.argv[2] || 100_000);
const recipe = {
  name: 'BenchmarkDTO',
  mappings: [
    { source: 'id', target: 'userId' },
    { source: 'profile.first_name | trim | lowercase', target: 'firstName' },
    { source: 'profile.last_name | trim | lowercase', target: 'lastName' },
    { source: 'profile.address.city', target: 'city' },
    { source: 'contact.emails[0]', target: 'primaryEmail' },
    { source: 'contact.phone | to_string | trim', target: 'phone' },
    { source: 'account.score', target: 'scoreNum', cast: 'decimal' },
    { source: 'account.tier', target: 'tierUpper', transform: 'uppercase' },
    { source: 'roles[*].name', target: 'roleNames' },
  ],
  required: ['userId', 'firstName', 'primaryEmail'],
};

const data = [];
for (let i = 0; i < count; i++) {
  data.push({
    id: `USR-${String(i).padStart(7, '0')}`,
    profile: { first_name: '  Alexander  ', last_name: '  Hamilton  ', address: { city: 'New York', state: 'NY', zip: '10001' } },
    contact: { emails: [`alex_${i}@example.com`, 'hamilton@treasury.gov'], phone: 18005550199 },
    account: { score: '98.50', tier: 'platinum', is_active: true },
    roles: [{ name: 'finance' }, { name: 'government' }, { name: 'admin' }],
  });
}

function plainJs(items) {
  return items.map((r) => ({
    userId: r.id,
    firstName: r.profile.first_name.trim().toLowerCase(),
    lastName: r.profile.last_name.trim().toLowerCase(),
    city: r.profile.address.city,
    primaryEmail: r.contact.emails[0],
    phone: String(r.contact.phone).trim(),
    scoreNum: Number(r.account.score),
    tierUpper: r.account.tier.toUpperCase(),
    roleNames: r.roles.map((x) => x.name),
  }));
}

// A ~25-line *generic* recipe interpreter in plain JS (paths compiled once), for a fair comparison.
const FN = { trim: (v) => String(v).trim(), lowercase: (v) => String(v).toLowerCase(), uppercase: (v) => String(v).toUpperCase(),
  to_string: (v) => String(v), decimal: (v) => Number(v) };
function compile(recipe) {
  return recipe.mappings.map((m) => {
    const [path, ...inline] = m.source.split('|').map((s) => s.trim());
    const fns = [...inline, ...(m.cast || m.transform ? (m.cast || m.transform).split('|').map((s) => s.trim()) : [])].map((f) => FN[f]);
    const segs = path.split('.').flatMap((p) => { const r = /^(\w+)(?:\[(\*|-?\d+)\])?$/.exec(p); return r[2] === undefined ? [r[1]] : [r[1], r[2]]; });
    const get = (o, i = 0) => {
      if (o == null || i === segs.length) return o;
      const s = segs[i];
      if (s === '*') return Array.isArray(o) ? o.map((x) => get(x, i + 1)) : undefined;
      if (/^-?\d+$/.test(s)) { const n = +s; return get(o[n < 0 ? o.length + n : n], i + 1); }
      return get(o[s], i + 1);
    };
    return { target: m.target, get, fns };
  });
}
function genericJs(recipe, items) {
  const c = compile(recipe);
  return items.map((r) => {
    const out = {};
    for (const m of c) { let v = m.get(r); if (v === undefined) continue; for (const f of m.fns) v = Array.isArray(v) ? v.map(f) : f(v); out[m.target] = v; }
    for (const k of recipe.required || []) if (!(k in out)) throw new Error('missing ' + k);
    return out;
  });
}

const engine = await DTOEngine.init();
const time = (label, fn, runs = 5) => {
  fn(); // warmup
  const ts = [];
  let out;
  for (let i = 0; i < runs; i++) { const t = performance.now(); out = fn(); ts.push(performance.now() - t); }
  ts.sort((a, b) => a - b);
  const med = ts[Math.floor(runs / 2)];
  console.log(`${label.padEnd(34)} median ${med.toFixed(1).padStart(8)} ms  -> ${Math.round(count / (med / 1000)).toLocaleString().padStart(11)} rec/s`);
  return out;
};

console.log(`records: ${count.toLocaleString()}`);
const a = time('WASM SDK  engine.transform()', () => engine.transform(recipe, data));
const b = time('Plain JS  Array.map()', () => plainJs(data));
const g = time('Generic JS recipe interpreter', () => genericJs(recipe, data));


// Correctness: same records, same keys/values (ignore key order)
const norm = (o) => JSON.stringify(Object.keys(o).sort().reduce((m, k) => ((m[k] = o[k]), m), {}));
assert.equal(a.length, b.length);
for (let i = 0; i < a.length; i++) assert.equal(norm(a[i]), norm(b[i]), `mismatch at ${i}`);
console.log('outputs identical: yes');
for (let i = 0; i < g.length; i++) assert.equal(norm(g[i]), norm(b[i]), `generic mismatch at ${i}`);
console.log("generic JS output identical: yes");
