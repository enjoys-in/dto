// One-shot transform(recipe, data) vs compile() + transform(data), for one
// large batch and for many one-record calls.
// Usage: node packages/js/bench/compile_vs_oneshot.mjs
import { DTOEngine } from '../src/index.js';
const recipe = { name: 'B', mappings: [
  { source: 'id', target: 'userId' }, { source: 'profile.first_name | trim | lowercase', target: 'firstName' },
  { source: 'profile.last_name | trim | lowercase', target: 'lastName' }, { source: 'profile.address.city', target: 'city' },
  { source: 'contact.emails[0]', target: 'primaryEmail' }, { source: 'contact.phone | to_string | trim', target: 'phone' },
  { source: 'account.score', target: 'scoreNum', cast: 'decimal' }, { source: 'account.tier', target: 'tierUpper', transform: 'uppercase' },
  { source: 'roles[*].name', target: 'roleNames' } ], required: ['userId', 'firstName', 'primaryEmail'] };
const rec = (i) => ({ id: `USR-${i}`, profile: { first_name: '  Alexander  ', last_name: '  Hamilton  ', address: { city: 'New York' } },
  contact: { emails: [`a${i}@x.com`], phone: 18005550199 }, account: { score: '98.50', tier: 'platinum' }, roles: [{ name: 'finance' }, { name: 'admin' }] });
const engine = await DTOEngine.init();
const time = (label, n, fn) => { fn(); const t = performance.now(); fn(); const ms = performance.now() - t; console.log(`${label.padEnd(44)} ${ms.toFixed(0).padStart(6)} ms -> ${Math.round(n / (ms / 1000)).toLocaleString('en-US').padStart(10)} rec/s`); };
const big = Array.from({ length: 100000 }, (_, i) => rec(i));
const compiled = engine.compile(recipe);
time('100k records, one call: transform(recipe,..)', 1e5, () => engine.transform(recipe, big));
time('100k records, one call: compiled.transform', 1e5, () => compiled.transform(big));
const small = Array.from({ length: 20000 }, (_, i) => rec(i));
time('20k calls x 1 record: transform(recipe,..)', 2e4, () => { for (const r of small) engine.transform(recipe, r); });
time('20k calls x 1 record: compiled.transform', 2e4, () => { for (const r of small) compiled.transform(r); });
compiled.free();
