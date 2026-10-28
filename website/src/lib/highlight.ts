// A small syntax highlighter for the few languages the site shows. It runs
// while pages are prerendered, so no highlighting code reaches the browser.

export type CodeLang = 'js' | 'rust' | 'sh' | 'toml' | 'text';

type Rule = [kind: string, pattern: RegExp];

const STRING = /"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`/;

const RULES: Record<Exclude<CodeLang, 'text'>, Rule[]> = {
	js: [
		['comment', /\/\/[^\n]*|\/\*[\s\S]*?\*\//],
		['string', STRING],
		['number', /\b\d[\d_]*(?:\.\d+)?\b/],
		[
			'keyword',
			/\b(?:const|let|var|function|return|if|else|for|of|in|new|import|from|export|await|async|class|extends|while|try|catch|throw|true|false|null|undefined|typeof)\b/
		],
		['type', /\b[A-Z][A-Za-z0-9_]*\b/],
		['fn', /\b[a-z_$][\w$]*(?=\s*\()/]
	],
	rust: [
		['comment', /\/\/[^\n]*/],
		['string', /b?"(?:[^"\\]|\\.)*"/],
		['macro', /\b[a-z_][a-z0-9_]*!/],
		['number', /\b\d[\d_]*(?:\.\d+)?\b/],
		[
			'keyword',
			/\b(?:use|let|mut|fn|pub|struct|enum|impl|match|if|else|while|for|in|loop|return|async|await|move|ref|as|where|self|Self|crate|mod|const|static|true|false|Some|None|Ok|Err)\b/
		],
		['type', /\b[A-Z][A-Za-z0-9_]*\b/],
		['fn', /\b[a-z_][a-z0-9_]*(?=\s*\()/]
	],
	sh: [
		['comment', /(?<=^|\s)#[^\n]*/],
		['string', STRING],
		['variable', /\b[A-Z_][A-Z0-9_]*(?==)|\$\{?[A-Za-z_][A-Za-z0-9_]*\}?/],
		['flag', /(?<=\s)--?[A-Za-z][\w-]*/],
		['keyword', /(?<=^|\n|&&\s|\|\s)(?:cargo|cd|docker|npx|npm|bun|sh|git|curl|rustup|wrangler)\b/]
	],
	toml: [
		['comment', /#[^\n]*/],
		['type', /^\[[^\]\n]+\]/m],
		['string', /"(?:[^"\\\n]|\\.)*"/],
		['keyword', /\b(?:true|false)\b/],
		['variable', /^[A-Za-z0-9_-]+(?=\s*=)|(?<=[{,]\s*)[A-Za-z0-9_-]+(?=\s*=)/m]
	]
};

const ESCAPES: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' };

export function escapeHtml(text: string): string {
	return text.replace(/[&<>"]/g, (c) => ESCAPES[c]);
}

/** Returns `code` as HTML, with tokens wrapped in `<span class="tok-…">`. */
export function highlight(code: string, lang: CodeLang): string {
	if (lang === 'text') return escapeHtml(code);
	const rules = RULES[lang];
	const pattern = new RegExp(rules.map(([, re]) => `(${re.source})`).join('|'), 'gm');
	let out = '';
	let last = 0;
	for (const match of code.matchAll(pattern)) {
		const index = match.slice(1).findIndex((group) => group !== undefined);
		if (index < 0 || match[0] === '') continue;
		out += escapeHtml(code.slice(last, match.index));
		out += `<span class="tok-${rules[index][0]}">${escapeHtml(match[0])}</span>`;
		last = match.index + match[0].length;
	}
	return out + escapeHtml(code.slice(last));
}
