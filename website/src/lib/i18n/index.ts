import { en, type Dict } from './en';
import { zh } from './zh';

export type Lang = 'en' | 'zh';

export const LANGS: readonly Lang[] = ['en', 'zh'];

/** BCP 47 tags, for `<html lang>`, hreflang and Open Graph. */
export const LOCALE: Record<Lang, { tag: string; og: string; name: string }> = {
	en: { tag: 'en', og: 'en_US', name: 'English' },
	zh: { tag: 'zh-CN', og: 'zh_CN', name: '中文' }
};

const dicts: Record<Lang, Dict> = { en, zh };

export function dict(lang: Lang): Dict {
	return dicts[lang];
}

/** English lives at the root, Chinese under `/zh`. */
export function langOf(pathname: string): Lang {
	return pathname === '/zh' || pathname.startsWith('/zh/') ? 'zh' : 'en';
}

/** The path without its language prefix: `/zh/docs` → `/docs`, `/zh` → `/`. */
export function unlocalize(pathname: string): string {
	return langOf(pathname) === 'zh' ? pathname.slice(3) || '/' : pathname;
}

/** A site path in the given language: `('/docs', 'zh')` → `/zh/docs`. */
export function localize(path: string, lang: Lang): string {
	if (lang === 'en') return path;
	return path === '/' ? '/zh' : `/zh${path}`;
}

/** Pages that exist in English only (the normative specifications). */
export const ENGLISH_ONLY = ['/spec', '/spec/groups'];
