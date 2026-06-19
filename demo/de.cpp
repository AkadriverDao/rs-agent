#include <iostream>

// Partition function: places pivot in correct position,
// elements smaller than pivot on the left, larger on the right.
int partition(int arr[], int low, int high) {
    int pivot = arr[high]; // choose last element as pivot
    int i = low - 1;       // index of smaller element

    for (int j = low; j < high; ++j) {
        if (arr[j] <= pivot) {
            ++i;
            std::swap(arr[i], arr[j]);
        }
    }
    std::swap(arr[i + 1], arr[high]);
    return i + 1;
}

// QuickSort recursive implementation
void quickSort(int arr[], int low, int high) {
    if (low < high) {
        int pi = partition(arr, low, high); // partition index

        quickSort(arr, low, pi - 1);  // sort left subarray
        quickSort(arr, pi + 1, high); // sort right subarray
    }
}

// Helper function to print an array
void printArray(const int arr[], int n) {
    for (int i = 0; i < n; ++i)
        std::cout << arr[i] << " ";
    std::cout << std::endl;
}

int main() {
    int arr[] = {10, 7, 8, 9, 1, 5};
    int n = sizeof(arr) / sizeof(arr[0]);

    std::cout << "Original array: ";
    printArray(arr, n);

    quickSort(arr, 0, n - 1);

    std::cout << "Sorted array:   ";
    printArray(arr, n);

    return 0;
}
