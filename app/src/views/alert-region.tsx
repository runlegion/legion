import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import type { FetchFailure } from "@/lib/api";

export interface AlertRegionProps {
  failures: (FetchFailure | null | undefined)[];
}

/**
 * The inline error region. Alert's guidance is to never stack alerts but to
 * prioritise the most important, so the first recorded failure is the one
 * shown; it persists until the request behind it succeeds.
 */
export function AlertRegion({ failures }: AlertRegionProps) {
  const shown = failures.find((entry): entry is FetchFailure => Boolean(entry));
  if (!shown) return null;
  return (
    <Alert variant="destructive">
      <AlertTitle>{shown.request} failed</AlertTitle>
      <AlertDescription>{shown.message}</AlertDescription>
    </Alert>
  );
}
