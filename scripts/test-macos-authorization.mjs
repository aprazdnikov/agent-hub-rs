import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const script = readFileSync(
  new URL('../crates/hub-app/src/autostart/macos-authorization.js', import.meta.url),
  'utf8',
);
const plist = "/tmp/中文 & ' $(not-code)/fixture.plist";

// Only the Objective-C boundary is replaced. Execute the exact embedded script.
// Native macOS 26 returns a string and exposes no SMAppServiceStatus constants.
function query(status, available = true) {
  return runInNewContext(`${script}\nrun([plist])`, {
    plist,
    ObjC: { import() {} },
    $: {
      NSClassFromString(name) {
        assert.equal(name, 'SMAppService');
        return {
          isNil: () => !available,
          statusForLegacyURL(url) {
            assert.ok(available, 'legacy fallback must not query the missing API');
            assert.equal(url.path, plist);
            if (status instanceof Error) throw status;
            return status;
          },
        };
      },
      NSURL: { fileURLWithPath: path => ({ path }) },
    },
  });
}

for (const [status, expected] of [
  [0, 'not-registered'],
  [1, 'enabled'],
  [2, 'requires-approval'],
  [3, 'not-found'],
  ['0', 'not-registered'],
  ['1', 'enabled'],
  ['2', 'requires-approval'],
  ['3', 'not-found'],
]) {
  test(`maps ${typeof status} ${status} without exported enum constants`, () => {
    assert.equal(query(status), expected);
  });
}

for (const status of [
  undefined, null, true, false, '', ' ', '01', '1.0', '1junk', 'enabled',
  '4', 4, -1, 1.5, NaN, Infinity, {}, [], { toString: () => '1' },
]) {
  test(`rejects unrecognized response: ${typeof status} ${String(status)}`, () => {
    assert.throws(() => query(status), /Service Management/);
  });
}

test('uses legacy behavior only when SMAppService is absent', () => {
  assert.equal(query(undefined, false), 'legacy');
});

test('propagates API errors instead of reporting enabled', () => {
  const error = new Error('native API failed');
  assert.throws(() => query(error), candidate => candidate === error);
});
