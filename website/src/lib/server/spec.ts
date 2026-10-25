import { Marked, type Tokens } from 'marked';
import { escapeHtml } from '#lib/highlight.ts';
import { REPO_URL } from '#lib/site.ts';

export interface TocEntry {
	id: string;
	label: string;
	depth: number;
}

const slug = (text: string) =>
	text
		.toLowerCase()
		.replace(/<[^>]+>/g, '')
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-|-$/g, '');

/** `5.3 The stream` → `sec-5-3`; unnumbered headings get a slug of their text. */
function headingId(text: string): string {
	const numbered = /^(\d+(?:\.\d+)*)\.?\s/.exec(text);
	return numbered ? `sec-${numbered[1].replace(/\./g, '-')}` : slug(text);
}

/** The repository's specifications that the site shows, and where. */
export const DOCUMENTS: Record<string, string> = {
	'SPEC.md': '/spec',
	'GROUP.md': '/spec/groups'
};

const sectionId = (number: string) => `sec-${number.replace(/\./g, '-')}`;

/** Renders a specification (SPEC.md, GROUP.md): anchors on headings, links for
 * `§n.n` references (`SPEC.md §n.n` into that page), links to the other
 * specifications on the site, and other repository-relative links pointed at
 * the repository (or dropped). */
export function renderSpec(markdown: string): { html: string; toc: TocEntry[] } {
	const toc: TocEntry[] = [];
	const marked = new Marked({
		gfm: true,
		renderer: {
			heading({ tokens, depth, text }: Tokens.Heading) {
				const inner = this.parser.parseInline(tokens);
				if (depth === 1) return `<h1>${inner}</h1>\n`;
				const id = headingId(text);
				if (depth <= 3) toc.push({ id, label: text.replace(/`/g, ''), depth });
				return `<h${depth} id="${id}"><a class="anchor" href="#${id}" aria-hidden="true" tabindex="-1">#</a>${inner}</h${depth}>\n`;
			},
			link({ href, tokens }: Tokens.Link) {
				const inner = this.parser.parseInline(tokens);
				if (/^[a-z]+:/i.test(href) || href.startsWith('#')) {
					const external = href.startsWith('#') ? '' : ' rel="external"';
					return `<a href="${escapeHtml(href)}"${external}>${inner}</a>`;
				}
				// Another specification shown on the site.
				const [file, hash] = href.split('#');
				if (file in DOCUMENTS) {
					return `<a href="${escapeHtml(DOCUMENTS[file] + (hash ? `#${hash}` : ''))}">${inner}</a>`;
				}
				// A file in the repository.
				return REPO_URL
					? `<a href="${escapeHtml(`${REPO_URL}/blob/main/${href}`)}" rel="external">${inner}</a>`
					: inner;
			}
		}
	});
	const body = marked.parse(markdown, { async: false });
	// Section references: into this document ("§5.3"), into another one on the
	// site ("SPEC.md §4.1"), but not into those elsewhere ("RFC 4648 §5").
	const html = body
		.replace(/<table>/g, '<div class="table"><table>')
		.replace(/<\/table>/g, '</table></div>')
		.replace(
			/(RFC \d{1,5} |([A-Z]+\.md) )?§(\d+(?:\.\d+)*)/g,
			(match, prefix: string | undefined, file: string | undefined, number: string) => {
				if (!prefix) return `<a href="#${sectionId(number)}">${match}</a>`;
				if (file && file in DOCUMENTS)
					return `<a href="${DOCUMENTS[file]}#${sectionId(number)}">${match}</a>`;
				return match;
			}
		);
	return { html, toc };
}
