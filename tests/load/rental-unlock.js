// k6: бенчмарк разблокировки самоката. Цель — p95 < 2 сек.
// Что меряем отдельно: hold YooKassa vs MQTT round-trip (см. ADR-0002).
// Запуск: k6 run --vus 50 --duration 2m tests/load/rental-unlock.js
import http from 'k6/http';
import { check } from 'k6';

export const options = {
  vus: 50,
  duration: '2m',
  thresholds: {
    http_req_duration: ['p(95)<2000'],
    http_req_failed: ['rate<0.01'],
  },
};

const BASE = __ENV.BASE_URL || 'http://localhost:9000';

export default function () {
  const res = http.post(
    `${BASE}/api/v1/rentals/__bench__/start`,
    JSON.stringify({ scooter_id: '00000000-0000-0000-0000-000000000000' }),
    { headers: { 'Content-Type': 'application/json' } },
  );
  check(res, { 'unlock accepted': (r) => r.status === 201 || r.status === 404 });
}
