// Tags, as a person means them: "Boats" and "boats" are one tag.

/** `have` with every tag of `more` it does not already carry, in any case. */
export function mergeTags(have: string[], more: readonly string[]): string[] {
  const out = [...have]
  for (const tag of more) {
    if (!out.some((t) => t.toLowerCase() === tag.toLowerCase())) out.push(tag)
  }
  return out
}
