import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import type { Document } from "@/lib/documents";
import { documentBody, documentTitle } from "@/lib/tree";
import { useDocumentSearch } from "@/hooks/use-document-search";
import { AlertRegion } from "@/views/alert-region";

/** How much body text feeds the local filter before it is truncated. */
const HAYSTACK_BODY_LIMIT = 2000;

/**
 * Command filters its own options by substring against each item's `value`.
 * The daemon's index matches body text too, so the value carries the same text
 * the server matched on -- otherwise a body-only hit would arrive and then be
 * hidden by the local filter.
 */
function haystack(doc: Document): string {
  return [documentTitle(doc), doc.id, doc.doc_type, documentBody(doc).slice(0, HAYSTACK_BODY_LIMIT)]
    .filter((part) => part.length > 0)
    .join(" ");
}

export interface SearchBoxProps {
  repo: string;
  onSelect: (doc: Document) => void;
}

/** The top bar's search box: queries GET /api/search and opens the pick. */
export function SearchBox({ repo, onSelect }: SearchBoxProps) {
  const { query, setQuery, results, loading, error } = useDocumentSearch(repo);
  const active = query.trim().length > 0;

  return (
    <Command value={query} onValueChange={setQuery} label={`Search ${repo} documents`}>
      <CommandInput placeholder="Search documents" />
      {active && (
        <CommandList>
          <CommandEmpty>{loading ? "Searching..." : "No documents match."}</CommandEmpty>
          {results.map((doc) => (
            <CommandItem
              key={doc.id}
              value={haystack(doc)}
              onSelect={() => {
                onSelect(doc);
                // Opening the document ends the search; clearing the query
                // empties the result list rather than leaving it over the work
                // area.
                setQuery("");
              }}
            >
              {documentTitle(doc)}
            </CommandItem>
          ))}
        </CommandList>
      )}
      <AlertRegion failures={[error]} />
    </Command>
  );
}
