import { useDocuments } from "@/hooks/use-documents";

function App() {
  const { documents, error } = useDocuments();
  return (
    <main>
      {error && (
        <p role="alert">
          {error.request} failed: {error.message}
        </p>
      )}
      <p>{documents.length} documents loaded.</p>
    </main>
  );
}

export default App;
