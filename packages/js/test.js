import test from 'node:test';
import assert from 'node:assert/strict';
import { DTOEngine } from './src/index.js';

test('JS/TS DTOEngine Recipe transformation', async () => {
  const engine = await DTOEngine.init();

  const recipe = {
    name: 'UserSummary',
    mappings: [
      { source: 'account.profile.name', target: 'name' },
      { source: 'account.profile.address.city', target: 'city' },
      { source: 'account.contact.emails[0]', target: 'primaryEmail' },
    ],
    required: ['name'],
  };

  const data = {
    account: {
      profile: {
        name: 'JavaScript Developer',
        address: { city: 'Tokyo' },
      },
      contact: {
        emails: ['js@example.jp'],
      },
    },
  };

  const result = engine.transform(recipe, data);
  assert.equal(result.name, 'JavaScript Developer');
  assert.equal(result.city, 'Tokyo');
  assert.equal(result.primaryEmail, 'js@example.jp');
});

test('JS/TS DTOEngine Batch Array transformation', async () => {
  const engine = await DTOEngine.init();

  const recipe = {
    name: 'BatchMapping',
    mappings: [
      { source: 'id', target: 'key' },
      { source: 'name', target: 'title', transform: 'uppercase' },
    ],
  };

  const data = [
    { id: 101, name: 'product a' },
    { id: 102, name: 'product b' },
  ];

  const result = engine.transform(recipe, data);
  assert.equal(result.length, 2);
  assert.deepEqual(result[0], { key: 101, title: 'PRODUCT A' });
  assert.deepEqual(result[1], { key: 102, title: 'PRODUCT B' });
});

test('JS/TS DTOEngine Medallion Pipeline multi-table transformation', async () => {
  const engine = await DTOEngine.init();

  const schema = {
    targets: [
      {
        table: 'silver_part',
        grain: 'header',
        mappings: [
          { source: 'item_id', target: 'part_number', cast: 'string' },
          { source: 'price', target: 'standard_cost', cast: 'decimal' },
        ],
      },
      {
        table: 'silver_orders',
        grain: 'line',
        business_key: ['po_id'],
        dedup_strategy: 'latest',
        mappings: [
          { source: 'orders[*].id', target: 'po_id', unpack: 'array_explode' },
          { source: 'item_id', target: 'part_number' },
          { source: 'orders[*].qty', target: 'quantity', cast: 'decimal', unpack: 'array_explode' },
        ],
      },
    ],
  };

  const data = {
    item_id: 'PART-100',
    price: 45.5,
    orders: [
      { id: 'PO-1', qty: 10 },
      { id: 'PO-2', qty: 25 },
    ],
  };

  const result = engine.transform(schema, data);
  assert.equal(result.silver_part.length, 1);
  assert.equal(result.silver_part[0].part_number, 'PART-100');
  assert.equal(result.silver_orders.length, 2);
  assert.equal(result.silver_orders[0].part_number, 'PART-100');
  assert.equal(result.silver_orders[0].quantity, 10);
  assert.equal(result.silver_orders[1].quantity, 25);
});

test('JS/TS compile once, transform many, free', async () => {
  const engine = await DTOEngine.init();
  const compiled = engine.compile({ name: 'T', mappings: [{ source: 'user.name', target: 'name' }] });
  assert.deepEqual(compiled.transform({ user: { name: 'Ada' } }), { name: 'Ada' });
  assert.deepEqual(compiled.transform([{ user: { name: 'A' } }, { user: { name: 'B' } }]), [{ name: 'A' }, { name: 'B' }]);
  assert.equal(compiled.transformRaw('{"user":{"name":"Z"}}'), '{"name":"Z"}');
  compiled.free();
  compiled.free();
  assert.throws(() => compiled.transform({}), /has been freed/);
});

test('JS/TS errors are thrown and outputs named "error" are not', async () => {
  const engine = await DTOEngine.init();
  // A real output with a single "error" field used to be mistaken for a failure.
  assert.deepEqual(
    engine.transform({ name: 'T', mappings: [{ source: 'msg', target: 'error' }] }, { msg: 'disk full' }),
    { error: 'disk full' }
  );
  assert.throws(
    () => engine.transform({ name: 'T', mappings: [{ source: 'id', target: 'id' }], required: ['id'] }, [{ id: 1 }, {}]),
    /item 1: required field 'id'/
  );
  assert.throws(() => engine.compile({ name: 'T', mappings: [{ source: 'a', target: 'b', cast: 'nope' }] }), /nope/);
});
