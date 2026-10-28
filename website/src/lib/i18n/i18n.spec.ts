import { describe, expect, it } from 'vitest';
import { en } from './en';
import { LANGS, dict, langOf, localize, unlocalize } from './index';
import { zh } from './zh';

describe('language routing', () => {
	it('reads the language from the path', () => {
		expect(langOf('/')).toBe('en');
		expect(langOf('/docs')).toBe('en');
		expect(langOf('/zh')).toBe('zh');
		expect(langOf('/zh/docs')).toBe('zh');
		expect(langOf('/zhx')).toBe('en');
	});

	it('maps paths to and from each language', () => {
		for (const path of ['/', '/docs', '/demo']) {
			for (const lang of LANGS) {
				const localized = localize(path, lang);
				expect(langOf(localized)).toBe(lang);
				expect(unlocalize(localized)).toBe(path);
			}
		}
		expect(localize('/', 'zh')).toBe('/zh');
		expect(localize('/docs', 'zh')).toBe('/zh/docs');
	});
});

describe('dictionaries', () => {
	// Walks both dictionaries in step, so that a missing, extra or empty
	// string in either is reported with its path.
	function compare(a: unknown, b: unknown, path: string, problems: string[]) {
		if (typeof a === 'string') {
			if (typeof b !== 'string' || !b.trim()) return problems.push(`${path}: missing in zh`);
			if (!a.trim()) problems.push(`${path}: empty in en`);
			const placeholders = (s: string) => (s.match(/\{\w+\}/g) ?? []).sort().join();
			if (placeholders(a) !== placeholders(b)) problems.push(`${path}: placeholders differ`);
			return;
		}
		if (Array.isArray(a)) {
			if (!Array.isArray(b) || a.length !== b.length)
				return problems.push(`${path}: length differs`);
			a.forEach((item, i) => compare(item, b[i], `${path}[${i}]`, problems));
			return;
		}
		const keys = new Set([...Object.keys(a as object), ...Object.keys(b as object)]);
		for (const key of keys) {
			compare(
				(a as Record<string, unknown>)[key],
				(b as Record<string, unknown>)[key],
				`${path}.${key}`,
				problems
			);
		}
	}

	it('have the same strings in both languages', () => {
		const problems: string[] = [];
		compare(en, zh, 'dict', problems);
		expect(problems).toEqual([]);
		expect(dict('zh')).toBe(zh);
	});

	it('keep page descriptions to a length search engines show', () => {
		for (const lang of LANGS) {
			for (const [page, meta] of Object.entries(dict(lang).meta)) {
				if (typeof meta === 'string' || page === 'notFound') continue;
				const limit = lang === 'zh' ? 100 : 170;
				expect(meta.description.length, `${lang} ${page}`).toBeLessThanOrEqual(limit);
				expect(meta.title.length, `${lang} ${page}`).toBeLessThanOrEqual(70);
			}
		}
	});
});
