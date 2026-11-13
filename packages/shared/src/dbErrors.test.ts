import { describe, expect, it } from "vitest";
import { isTransientDbError } from "./dbErrors.js";

function pgError(code: string, message = "x") {
  return Object.assign(new Error(message), { code });
}

describe("isTransientDbError", () => {
  it.each(["57P01", "57P03", "25006", "08006"])(
    "treats SQLSTATE %s as transient",
    (code) => {
      expect(isTransientDbError(pgError(code))).toBe(true);
    },
  );

  it("treats socket errors as transient", () => {
    expect(isTransientDbError(pgError("ECONNREFUSED"))).toBe(true);
    expect(
      isTransientDbError(new Error("Connection terminated unexpectedly")),
    ).toBe(true);
  });

  it("looks through drizzle's wrapper to the driver error", () => {
    const wrapped = new Error("Failed query: select 1", {
      cause: pgError("57P03", "the database system is shutting down"),
    });
    expect(isTransientDbError(wrapped)).toBe(true);
  });

  it("leaves real query errors alone", () => {
    expect(isTransientDbError(pgError("23505"))).toBe(false); // unique_violation
    expect(isTransientDbError(pgError("42P01"))).toBe(false); // undefined_table
    expect(isTransientDbError(new Error("boom"))).toBe(false);
    expect(isTransientDbError(undefined)).toBe(false);
    expect(isTransientDbError("ECONNREFUSED")).toBe(false);
  });
});
