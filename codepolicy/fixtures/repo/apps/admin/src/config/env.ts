// Reading env in the config layer is allowed (path excluded by NO_DIRECT_ENV_ACCESS).
export const config = {
  apiKey: process.env.API_KEY,
};
