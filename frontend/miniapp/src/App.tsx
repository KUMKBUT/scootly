import { HistoryPage } from './features/history/HistoryPage';

export default function App() {
  return (
    <div className="min-h-screen bg-neutral-50">
      <header className="px-4 pb-2 pt-6">
        <h1 className="text-2xl font-bold text-neutral-900">Scootly</h1>
        <p className="text-sm text-neutral-500">История поездок и платежей</p>
      </header>
      <main className="px-4 pb-8">
        <HistoryPage />
      </main>
    </div>
  );
}
