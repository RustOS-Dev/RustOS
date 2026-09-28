/* musl-threads: pthreads, mutexes, condition variables and __thread. */
#include <pthread.h>
#include <stdio.h>

#define N 4
static __thread int tls_value = 7;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
static int total, ready;

static void *worker(void *arg) {
    int id = (int)(long)arg;
    tls_value = id * 100;          /* each thread has its own copy */
    for (int i = 0; i < 10000; i++) {
        pthread_mutex_lock(&lock);
        total++;
        pthread_mutex_unlock(&lock);
    }
    pthread_mutex_lock(&lock);
    ready++;
    pthread_cond_signal(&cond);
    pthread_mutex_unlock(&lock);
    return (void *)(long)(tls_value + id);
}

int main(void) {
    pthread_t t[N];
    for (long i = 0; i < N; i++)
        if (pthread_create(&t[i], 0, worker, (void *)i)) {
            printf("pthread_create failed\n");
            return 1;
        }
    pthread_mutex_lock(&lock);
    while (ready < N)
        pthread_cond_wait(&cond, &lock);
    pthread_mutex_unlock(&lock);
    int ok = 1;
    for (long i = 0; i < N; i++) {
        void *r;
        pthread_join(t[i], &r);
        if ((long)r != i * 100 + i) ok = 0;
    }
    ok = ok && total == N * 10000 && tls_value == 7;
    printf("threads %s: total=%d main tls=%d\n", ok ? "OK" : "FAILED", total, tls_value);
    return !ok;
}
