// Auth-состояние (Zustand): access-JWT для API-клиента.
// Вход через Telegram init-data — отдельная auth-фича (бэкенд MVP #1 готов).
import { create } from 'zustand';

interface AuthState {
  accessToken: string | null;
  setAccessToken: (accessToken: string | null) => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  accessToken: null,
  setAccessToken: (accessToken) => set({ accessToken }),
}));
