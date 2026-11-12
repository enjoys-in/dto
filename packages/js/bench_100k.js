import { DTOEngine } from './src/index.js';

async function run() {
  const count = 10_000;
  console.log(`================================================================================`);
  console.log(`         JAVASCRIPT / NODE.JS (WASM) 100K RECORD ARRAY BENCHMARK               `);
  console.log(`================================================================================`);

  const initialMem = process.memoryUsage();
  console.log(`Initial RSS: ${(initialMem.rss / 1024 / 1024).toFixed(2)} MB`);

  const engine = await DTOEngine.init();

  const recipe = {
    name: 'BenchmarkDTO',
    version: '1.0.0',
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

  console.log(`Generating ${count.toLocaleString()} JSON records in Node.js...`);
  const t0 = performance.now();
  const data = [];
  for (let i = 0; i < count; i++) {
    data.push({
      id: `USR-${String(i).padStart(7, '0')}`,
      profile: {
        first_name: '  Alexander  ',
        last_name: '  Hamilton  ',
        address: { city: 'New York', state: 'NY', zip: '10001' },
      },
      contact: {
        emails: [`alex_${i}@example.com`, 'hamilton@treasury.gov'],
        phone: 18005550199,
      },
      account: { score: '98.50', tier: 'platinum', is_active: true },
      roles: [{ name: 'finance' }, { name: 'government' }, { name: 'admin' }],
    });
  }
  const genTime = performance.now() - t0;
  const postGenMem = process.memoryUsage();
  console.log(`Generated in ${genTime.toFixed(2)}ms. RSS: ${(postGenMem.rss / 1024 / 1024).toFixed(2)} MB`);

  console.log(`Transforming ${count.toLocaleString()} items via DTO WASM Engine...`);
  const tStart = performance.now();
  const output = engine.transform(recipe, data);
  const elapsed = performance.now() - tStart;
  const finalMem = process.memoryUsage();

  const throughput = (count / (elapsed / 1000)).toFixed(0);
  const latencyUs = ((elapsed * 1000) / count).toFixed(2);

  console.log(`\nResults:`);
  console.log(`  • Transformed Count   : ${output.length.toLocaleString()} records`);
  console.log(`  • Wall Clock Elapsed  : ${elapsed.toFixed(2)} ms (${(elapsed / 1000).toFixed(3)} s)`);
  console.log(`  • Throughput          : ${Number(throughput).toLocaleString()} records / sec`);
  console.log(`  • Average Latency     : ${latencyUs} µs / item`);
  console.log(`  • Peak Working Set RSS: ${(finalMem.rss / 1024 / 1024).toFixed(2)} MB`);
  console.log(`  • Heap Used           : ${(finalMem.heapUsed / 1024 / 1024).toFixed(2)} MB`);
  console.log(`================================================================================\n`);
}

run().catch(console.error);
