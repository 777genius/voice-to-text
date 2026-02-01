export enum AuthErrorCode {
  InvalidCredentials = 'INVALID_CREDENTIALS',
  EmailNotVerified = 'EMAIL_NOT_VERIFIED',
  UserNotFound = 'USER_NOT_FOUND',
  CodeInvalid = 'CODE_INVALID',
  CodeExpired = 'CODE_EXPIRED',
  RateLimited = 'RATE_LIMITED',
  RateLimitExceeded = 'RATE_LIMIT_EXCEEDED',
  NetworkError = 'NETWORK_ERROR',
  SessionExpired = 'SESSION_EXPIRED',
  PasswordWeak = 'PASSWORD_WEAK',
  ValidationError = 'VALIDATION_ERROR',
  OAuthError = 'OAUTH_ERROR',
  OAuthAccountLinked = 'OAUTH_ACCOUNT_ALREADY_LINKED',
  Unknown = 'UNKNOWN',
}

export class AuthError extends Error {
  constructor(
    public readonly code: AuthErrorCode,
    message: string,
    public readonly retryAfterMs?: number
  ) {
    super(message);
    this.name = 'AuthError';
  }
}
