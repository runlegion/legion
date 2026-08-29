import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import type { Document } from "@/lib/documents";
import { documentTitle } from "@/lib/tree";
import { useDocumentSearch } from "@/hooks/use-document-search";
import { AlertRegion } from "@/views/alert-region";

/**
 * Command filters its own options by substring against each item's `value`, but
 * the daemon has already ranked and scoped these results, and it matches on
 * stemmed terms -- a real hit for "rendering" need not contain that substring
 * anywhere. Re-filtering here would silently drop such hits, so each option's
 * value leads with the live query to pass the filter unconditionally, and the
 * document id after it keeps the value unique per option.
 */
function unfilteredValue(query: string, doc: Document): string {
  return `${query} ${doc.id}`;
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
              value={unfilteredValue(query, doc)}
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
