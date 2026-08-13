/**
 * Sessions are created with a slug for a name when the program didn't pick
 * one (`untitled-2026-08-14`, see the sessions table default). That is an
 * internal label, not a title, and putting it at the top of the page in
 * 24px reads like the product is broken.
 */

const PLACEHOLDER = /^untitled-\d{4}-\d{2}-\d{2}$/i;

/** The name worth showing the user, or null if there isn't one. */
export function displayName(name: string | null | undefined): string | null {
  const trimmed = name?.trim();
  if (!trimmed) return null;
  if (PLACEHOLDER.test(trimmed)) return null;
  return trimmed;
}
