// Минимальный HTTP-клиент на fetch — общий для miniapp и mobile.
// Ошибки API приходят единым конвертом openapi `Error` { code, message }
// и выбрасываются как ApiError с машинным code для UI.

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

export interface ApiConfig {
  /** Например `http://localhost:9000` (nginx → сервисы, см. openapi servers). */
  baseUrl: string;
  /** Access-JWT из стора приложения; без токена — запрос без Authorization. */
  getAccessToken: () => string | null | undefined;
}

/** Плоский набор query-параметров; undefined — параметр не попадает в URL. */
export type QueryParams = object;

export async function apiRequest<T>(
  config: ApiConfig,
  path: string,
  params?: QueryParams,
): Promise<T> {
  const url = `${config.baseUrl.replace(/\/+$/, '')}${path}${toQueryString(params)}`;
  const token = config.getAccessToken();

  const response = await fetch(url, {
    headers: token ? { Authorization: `Bearer ${token}` } : undefined,
  });
  if (!response.ok) {
    throw await toApiError(response);
  }
  return (await response.json()) as T;
}

/** Без URL/URLSearchParams — в React Native их полифиллов нет по умолчанию. */
function toQueryString(params?: QueryParams): string {
  const parts: string[] = [];
  for (const [key, value] of Object.entries(params ?? {})) {
    if (value !== undefined) {
      parts.push(`${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`);
    }
  }
  return parts.length > 0 ? `?${parts.join('&')}` : '';
}

async function toApiError(response: Response): Promise<ApiError> {
  let code = 'internal';
  let message = `HTTP ${response.status}`;
  try {
    const body = (await response.json()) as { code?: string; message?: string };
    if (body.code) code = body.code;
    if (body.message) message = body.message;
  } catch {
    // Тело не JSON (например, от nginx) — оставляем дефолты.
  }
  return new ApiError(response.status, code, message);
}
