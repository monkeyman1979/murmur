export function pluralCategory(count: number, locale: string): Intl.LDMLPluralRule {
  return new Intl.PluralRules(locale).select(count);
}

/** Selects a translated time unit for singular, Russian few, or other forms. */
export function timeUnitKey(unit: string, count: number, locale: string): string {
  const singular = unit.endsWith("s") ? unit.slice(0, -1) : unit;
  const category = pluralCategory(count, locale);
  return `timeUnits.${category === "one" ? singular : category === "few" ? `${singular}Few` : `${singular}s`}`;
}
