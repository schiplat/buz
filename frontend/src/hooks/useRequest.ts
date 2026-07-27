import { ref } from 'vue'

export function useRequest<T>(fetcher: () => Promise<T>, options: { immediate?: boolean } = {}) {
  const data = ref<T | null>(null)
  const loading = ref(false)
  const error = ref<Error | null>(null)

  async function execute() {
    loading.value = true
    error.value = null
    try {
      data.value = await fetcher()
    } catch (e) {
      error.value = e as Error
    } finally {
      loading.value = false
    }
  }

  if (options.immediate ?? true) {
    execute()
  }

  return { data, loading, error, execute }
}
