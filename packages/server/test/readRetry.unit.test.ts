import { describe, expect, it } from "vitest";
import { installReadRetry, isPlainRead } from "../src/db/readRetry.js";

const terminated = () => new Error("Connection terminated unexpectedly");

/** A stand-in pool whose query() fails `failures` times, then succeeds. */
function fakePool(failures: number, error: () => Error = terminated) {
  const pool = {
    calls: 0,
    async query(_config: unknown) {
      pool.calls++;
      if (pool.calls <= failures) throw error();
      return { rows: [] };
    },
  };
  return pool;
}

describe("isPlainRead", () => {
  it.each([
    ["select 1", true],
    ['  SELECT "id" from "sessions"', true],
    ['select * from "sessions" for update', false],
    ['update "sessions" set x = 1', false],
    ['insert into "sessions" values (1) returning id', false],
    ["with x as (delete from t returning *) select * from x", false],
    ["/* health */ select 1", false],
  ])("%s → %s", (text, expected) => {
    expect(isPlainRead(text)).toBe(expected);
    expect(isPlainRead({ text })).toBe(expected);
  });
});

describe("installReadRetry", () => {
  it("retries a SELECT through a transient connection error", async () => {
    const pool = fakePool(2);
    installReadRetry(pool, [0, 0, 0]);
    await expect(pool.query({ text: "select 1" })).resolves.toEqual({ rows: [] });
    expect(pool.calls).toBe(3);
  });

  it("gives up after the last delay", async () => {
    const pool = fakePool(10);
    installReadRetry(pool, [0, 0]);
    await expect(pool.query("select 1")).rejects.toThrow("Connection terminated");
    expect(pool.calls).toBe(3);
  });

  it("never replays a write", async () => {
    const pool = fakePool(1);
    installReadRetry(pool, [0, 0]);
    await expect(pool.query("update t set x = 1")).rejects.toThrow();
    expect(pool.calls).toBe(1);
  });

  it("doesn't retry real query errors", async () => {
    const pool = fakePool(1, () =>
      Object.assign(new Error("relation does not exist"), { code: "42P01" }),
    );
    installReadRetry(pool, [0, 0]);
    await expect(pool.query("select * from nope")).rejects.toThrow();
    expect(pool.calls).toBe(1);
  });
});
